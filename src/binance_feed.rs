use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use futures_util::StreamExt;
use serde::Deserialize;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

const BINANCE_WS: &str = "wss://stream.binance.com:9443/stream";
const SYMBOLS: &[&str] = &["btcusdt", "ethusdt", "solusdt"];
const BUFFER_SIZE: usize = 600;

#[derive(Debug, Clone, Copy)]
pub struct Tick {
    pub timestamp: f64,
    pub price: f64,
    pub volume: f64,
}

#[derive(Deserialize)]
struct BinanceStreamMsg {
    data: serde_json::Value,
}

pub type TickBuffers = Arc<RwLock<HashMap<String, VecDeque<Tick>>>>;

pub struct BinanceFeed {
    buffers: TickBuffers,
    running: Arc<AtomicBool>,
}

impl BinanceFeed {
    pub fn new() -> Self {
        let mut map = HashMap::new();
        for s in SYMBOLS {
            map.insert(s.to_uppercase(), VecDeque::with_capacity(BUFFER_SIZE));
        }
        Self {
            buffers: Arc::new(RwLock::new(map)),
            running: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Create a feed that writes to pre-existing shared buffers.
    pub fn with_buffers(buffers: TickBuffers) -> Self {
        Self {
            buffers,
            running: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn buffers(&self) -> TickBuffers {
        Arc::clone(&self.buffers)
    }

    pub async fn run(&self) {
        let streams = SYMBOLS
            .iter()
            .map(|s| format!("{s}@trade"))
            .collect::<Vec<_>>()
            .join("/");
        let url = format!("{BINANCE_WS}?streams={streams}");
        self.running.store(true, Ordering::SeqCst);

        while self.running.load(Ordering::SeqCst) {
            match tokio_tungstenite::connect_async(&url).await {
                Ok((ws, _)) => {
                    info!("Binance WebSocket connected");
                    let (_, mut read) = ws.split();

                    while let Some(msg) = read.next().await {
                        if !self.running.load(Ordering::Relaxed) {
                            break;
                        }
                        match msg {
                            Ok(tokio_tungstenite::tungstenite::Message::Text(text)) => {
                                self.handle(&text).await;
                            }
                            Ok(tokio_tungstenite::tungstenite::Message::Binary(bytes)) => {
                                if let Ok(text) = String::from_utf8(bytes.into()) {
                                    self.handle(&text).await;
                                }
                            }
                            Ok(tokio_tungstenite::tungstenite::Message::Close(_)) => {
                                warn!("Binance WS closed by server. Reconnecting in 2s…");
                                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                                break;
                            }
                            Err(e) => {
                                warn!("Binance WS error: {e}. Reconnecting in 3s…");
                                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                                break;
                            }
                            _ => {}
                        }
                    }
                }
                Err(e) => {
                    warn!("Binance WS connect failed: {e}. Reconnecting in 5s…");
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                }
            }
        }
    }

    async fn handle(&self, raw: &str) {
        let msg: BinanceStreamMsg = match serde_json::from_str(raw) {
            Ok(m) => m,
            Err(e) => {
                debug!("Feed parse error: {e}");
                return;
            }
        };

        let data = &msg.data;
        if data.get("e").and_then(|v| v.as_str()) != Some("trade") {
            return;
        }

        let symbol = match data.get("s").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => return,
        };
        let price: f64 = match data.get("p").and_then(|v| v.as_str()).and_then(|s| s.parse().ok()) {
            Some(p) => p,
            None => return,
        };
        let volume: f64 =
            match data.get("q").and_then(|v| v.as_str()).and_then(|s| s.parse().ok()) {
                Some(q) => q,
                None => return,
            };
        let ts: f64 = match data.get("T").and_then(|v| v.as_f64()) {
            Some(t) => t / 1000.0,
            None => return,
        };

        let mut bufs = self.buffers.write().await;
        if let Some(buf) = bufs.get_mut(&symbol) {
            if buf.len() == BUFFER_SIZE {
                buf.pop_front();
            }
            buf.push_back(Tick {
                timestamp: ts,
                price,
                volume,
            });
        }
    }
}

// ── Free functions for reading shared buffers ───────────────────────────────

pub async fn get_price(buffers: &TickBuffers, symbol: &str) -> Option<f64> {
    let bufs = buffers.read().await;
    bufs.get(&symbol.to_uppercase())
        .and_then(|b| b.back())
        .map(|t| t.price)
}

pub async fn get_ticks_since(buffers: &TickBuffers, symbol: &str, since_ts: f64) -> Vec<Tick> {
    let bufs = buffers.read().await;
    bufs.get(&symbol.to_uppercase())
        .map(|b| b.iter().filter(|t| t.timestamp >= since_ts).copied().collect())
        .unwrap_or_default()
}

pub async fn has_data(buffers: &TickBuffers, symbol: &str, min_ticks: usize) -> bool {
    let bufs = buffers.read().await;
    bufs.get(&symbol.to_uppercase())
        .map(|b| b.len() >= min_ticks)
        .unwrap_or(false)
}
