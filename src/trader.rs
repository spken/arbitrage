use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::binance_feed::{self, BinanceFeed, TickBuffers};
use crate::config;
use crate::edge_detector::EdgeDetector;
use crate::polymarket_client::PolymarketClient;
use crate::risk_manager::RiskManager;
use crate::settlement_monitor::SettlementMonitor;
use crate::types::TradeSignal;

const SCAN_INTERVAL: f64 = 0.5;
const COOLDOWN_SECONDS: f64 = 60.0;
const WARMUP_SECONDS: u64 = 10;
const WATCHDOG_INTERVAL: u64 = 2;
const STATUS_INTERVAL: u64 = 300;

#[derive(Clone)]
struct TrackedOrder {
    order_id: String,
    placed_at: f64,
    signal: TradeSignal,
}

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

/// All shared state is behind Arc so tasks can reference it independently.
struct SharedState {
    buffers: TickBuffers,
    poly: Arc<RwLock<PolymarketClient>>,
    risk: Arc<RwLock<RiskManager>>,
    settlement_pending: Arc<RwLock<HashMap<String, TradeSignal>>>,
    order_timestamps: Arc<RwLock<HashMap<String, TrackedOrder>>>,
    cooldowns: Arc<RwLock<HashMap<String, f64>>>,
    running: Arc<AtomicBool>,
}

pub struct Trader {
    state: Arc<SharedState>,
}

impl Trader {
    pub fn new(cfg: crate::config::Config) -> Self {
        let feed = BinanceFeed::new();
        let buffers = feed.buffers();

        Self {
            state: Arc::new(SharedState {
                buffers,
                poly: Arc::new(RwLock::new(PolymarketClient::new(&cfg))),
                risk: Arc::new(RwLock::new(RiskManager::new(cfg.starting_capital))),
                settlement_pending: Arc::new(RwLock::new(HashMap::new())),
                order_timestamps: Arc::new(RwLock::new(HashMap::new())),
                cooldowns: Arc::new(RwLock::new(HashMap::new())),
                running: Arc::new(AtomicBool::new(false)),
            }),
        }
    }

    pub async fn start(&self) {
        let s = &self.state;
        s.running.store(true, Ordering::SeqCst);

        {
            let risk = s.risk.read().await;
            info!("{}", "=".repeat(60));
            info!("Bot starting | Capital: ${:.2}", risk.starting_capital);
        }
        info!(
            "Risk: max_pos={:.0}% | daily_halt={:.0}% | kill={:.0}%",
            config::MAX_POSITION_PCT * 100.0,
            config::DAILY_LOSS_LIMIT_PCT * 100.0,
            config::TOTAL_DRAWDOWN_KILL_PCT * 100.0,
        );
        info!(
            "Edge: threshold={:.0}% | momentum_window={:.0}s | order_cancel_timeout={:.0}s",
            config::MIN_EDGE_THRESHOLD * 100.0,
            config::MOMENTUM_WINDOW_SECONDS,
            config::ORDER_CANCEL_TIMEOUT_SECS,
        );
        info!("{}", "=".repeat(60));

        // Spawn Binance feed
        let feed_buffers = s.buffers.clone();
        let feed_running = s.running.clone();
        let feed_handle = tokio::spawn(async move {
            let feed = BinanceFeed::with_buffers(feed_buffers);
            while feed_running.load(Ordering::Relaxed) {
                feed.run().await;
            }
        });

        info!("Warming up price feed ({WARMUP_SECONDS}s)…");
        tokio::time::sleep(std::time::Duration::from_secs(WARMUP_SECONDS)).await;

        if let Some(btc) = binance_feed::get_price(&s.buffers, "BTCUSDT").await {
            info!("Feed active | BTC: ${btc:.2}");
        } else {
            warn!("No BTC price data after warmup. Check network.");
            return;
        }

        {
            let mut poly = s.poly.write().await;
            let count = poly.get_active_markets(true).await.len();
            info!("Loaded {count} active markets");

            if let Some(bal) = poly.get_balance().await {
                info!("Polymarket USDC balance: ${bal:.2}");
            }
        }

        // Spawn worker tasks
        let st = Arc::clone(&self.state);
        let scan_handle = tokio::spawn(async move { scan_loop(&st).await });

        let st = Arc::clone(&self.state);
        let watchdog_handle = tokio::spawn(async move { watchdog_loop(&st).await });

        let st = Arc::clone(&self.state);
        let status_handle = tokio::spawn(async move { status_loop(&st).await });

        let settlement = SettlementMonitor::new(
            Arc::clone(&s.poly),
            Arc::clone(&s.risk),
            Arc::clone(&s.settlement_pending),
            Arc::clone(&s.running),
        );
        let settlement_handle = tokio::spawn(async move { settlement.run().await });

        // Wait for ctrl+c
        tokio::signal::ctrl_c().await.ok();
        info!("Shutdown signal received.");

        // Signal all loops to stop
        s.running.store(false, Ordering::SeqCst);

        // Give tasks a moment to exit gracefully, then abort stragglers
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        for handle in [feed_handle, scan_handle, watchdog_handle, status_handle, settlement_handle] {
            handle.abort();
        }

        shutdown(s).await;
    }
}

// ── Free-standing task functions (operate on shared state) ──────────────────

async fn scan_loop(s: &SharedState) {
    let mut last_scan = 0.0_f64;

    while s.running.load(Ordering::Relaxed) {
        {
            let risk = s.risk.read().await;
            if !risk.is_live() {
                warn!("Risk limits breached. Trading halted.");
                s.running.store(false, Ordering::SeqCst);
                break;
            }
        }

        let now = now_secs();
        if now - last_scan >= SCAN_INTERVAL {
            last_scan = now;
            if let Err(e) = scan_markets(s).await {
                warn!("Scan error: {e}");
            }
        }

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

async fn scan_markets(s: &SharedState) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let markets = {
        let mut poly = s.poly.write().await;
        poly.get_active_markets(false).await.to_vec()
    };

    for market in &markets {
        {
            let mut risk = s.risk.write().await;
            if !risk.check_limits() {
                break;
            }
        }

        let cid = market.condition_id.as_deref().unwrap_or("");
        if cid.is_empty() {
            continue;
        }

        {
            let cooldowns = s.cooldowns.read().await;
            if now_secs() - cooldowns.get(cid).copied().unwrap_or(0.0) < COOLDOWN_SECONDS {
                continue;
            }
        }

        let signal = {
            let poly = s.poly.read().await;
            let mut risk = s.risk.write().await;
            EdgeDetector::scan_market(market, &s.buffers, &*poly, &mut *risk).await
        };

        if let Some(signal) = signal {
            execute(s, signal).await;
        }
    }

    Ok(())
}

async fn execute(s: &SharedState, signal: TradeSignal) {
    {
        let mut risk = s.risk.write().await;
        if !risk.check_limits() {
            return;
        }
    }

    let order_id = {
        let mut poly = s.poly.write().await;
        poly.place_order(&signal).await
    };

    if let Some(order_id) = order_id {
        let placed_at = now_secs();

        {
            let mut risk = s.risk.write().await;
            risk.record_open(&signal.token_id, signal.size_usdc);
        }
        {
            let mut pending = s.settlement_pending.write().await;
            pending.insert(signal.token_id.clone(), signal.clone());
        }
        {
            let mut cooldowns = s.cooldowns.write().await;
            cooldowns.insert(signal.condition_id.clone(), placed_at);
        }
        {
            let mut timestamps = s.order_timestamps.write().await;
            timestamps.insert(
                signal.token_id.clone(),
                TrackedOrder {
                    order_id,
                    placed_at,
                    signal,
                },
            );
        }
    } else {
        warn!("Order failed: {}", signal.market_id);
    }
}

async fn watchdog_loop(s: &SharedState) {
    while s.running.load(Ordering::Relaxed) {
        tokio::time::sleep(std::time::Duration::from_secs(WATCHDOG_INTERVAL)).await;
        let now = now_secs();

        let stale: Vec<(String, TrackedOrder)> = {
            let timestamps = s.order_timestamps.read().await;
            timestamps
                .iter()
                .filter(|(_, tracked)| now - tracked.placed_at > config::ORDER_CANCEL_TIMEOUT_SECS)
                .map(|(tid, tracked)| (tid.clone(), tracked.clone()))
                .collect()
        };

        for (token_id, tracked) in stale {
            warn!(
                "[WATCHDOG] Cancelling stale order | {} {} | id={} | age={:.1}s",
                tracked.signal.symbol,
                tracked.signal.direction.to_uppercase(),
                tracked.order_id,
                now - tracked.placed_at,
            );

            {
                let poly = s.poly.read().await;
                poly.cancel_order(&tracked.order_id).await;
            }
            {
                let mut timestamps = s.order_timestamps.write().await;
                timestamps.remove(&token_id);
            }
            {
                let mut poly = s.poly.write().await;
                poly.remove_order_tracking(&token_id);
            }
            {
                let mut pending = s.settlement_pending.write().await;
                pending.remove(&token_id);
            }
            {
                let mut risk = s.risk.write().await;
                risk.record_close(&token_id, 0.0, "cancelled", Some(&tracked.signal));
            }
        }
    }
}

async fn status_loop(s: &SharedState) {
    while s.running.load(Ordering::Relaxed) {
        tokio::time::sleep(std::time::Duration::from_secs(STATUS_INTERVAL)).await;
        let risk = s.risk.read().await;
        info!("[STATUS] {}", risk.summary());
    }
}

async fn shutdown(s: &SharedState) {
    info!("Cancelling open orders…");
    {
        let mut poly = s.poly.write().await;
        poly.cancel_all().await;
    }
    {
        let risk = s.risk.read().await;
        info!("Final: {}", risk.summary());
    }
    info!("Bot stopped.");
}
