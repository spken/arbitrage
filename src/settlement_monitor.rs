use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::config;
use crate::polymarket_client::PolymarketClient;
use crate::risk_manager::RiskManager;
use crate::types::TradeSignal;

pub struct SettlementMonitor {
    poly: Arc<RwLock<PolymarketClient>>,
    risk: Arc<RwLock<RiskManager>>,
    pub pending: Arc<RwLock<HashMap<String, TradeSignal>>>,
    running: Arc<AtomicBool>,
}

impl SettlementMonitor {
    pub fn new(
        poly: Arc<RwLock<PolymarketClient>>,
        risk: Arc<RwLock<RiskManager>>,
        pending: Arc<RwLock<HashMap<String, TradeSignal>>>,
        running: Arc<AtomicBool>,
    ) -> Self {
        Self {
            poly,
            risk,
            pending,
            running,
        }
    }

    pub async fn run(&self) {
        info!("Settlement monitor started");

        while self.running.load(Ordering::SeqCst) {
            tokio::time::sleep(std::time::Duration::from_secs(config::SETTLEMENT_POLL_SECONDS))
                .await;
            if let Err(e) = self.check_resolutions().await {
                warn!("Settlement check error: {e}");
            }
        }
    }

    async fn check_resolutions(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let pending_snapshot: Vec<String> = {
            let p = self.pending.read().await;
            if p.is_empty() {
                return Ok(());
            }
            p.keys().cloned().collect()
        };

        let positions = {
            let poly = self.poly.read().await;
            poly.get_positions().await
        };

        let open_token_ids: std::collections::HashSet<String> = positions
            .iter()
            .filter_map(|p| p.effective_token_id().map(|s| s.to_string()))
            .collect();

        let resolved: Vec<String> = pending_snapshot
            .into_iter()
            .filter(|tid| !open_token_ids.contains(tid))
            .collect();

        for token_id in resolved {
            let signal = {
                let mut p = self.pending.write().await;
                match p.remove(&token_id) {
                    Some(s) => s,
                    None => continue,
                }
            };
            self.settle(&token_id, &signal).await;
        }

        Ok(())
    }

    async fn settle(&self, token_id: &str, signal: &TradeSignal) {
        let resolved_data = {
            let poly = self.poly.read().await;
            poly.get_resolved_position(token_id).await
        };

        let (pnl, outcome) = if let Some(data) = resolved_data {
            let payout = data.payout;
            let outcome = if payout > 0.0 { "win" } else { "loss" };
            (payout - signal.size_usdc, outcome)
        } else {
            warn!(
                "Settlement data unavailable for {}…. Removing from tracking.",
                &token_id[..12.min(token_id.len())]
            );
            (0.0, "unknown")
        };

        {
            let mut risk = self.risk.write().await;
            risk.record_close(token_id, pnl, outcome, Some(signal));
        }

        let emoji = if pnl > 0.0 {
            "+"
        } else if pnl < 0.0 {
            "-"
        } else {
            "?"
        };
        info!(
            "{emoji} Settled | {} {} | pnl=${pnl:+.4} | outcome={outcome}",
            signal.symbol,
            signal.direction.to_uppercase()
        );
    }
}
