use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use tracing::{debug, info, warn};

use crate::config;
use crate::types::TradeSignal;

// ── API response types ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct Market {
    pub condition_id: Option<String>,
    pub question: Option<String>,
    pub active: Option<bool>,
    pub closed: Option<bool>,
    pub volume: Option<String>,
    pub tokens: Option<Vec<Token>>,
    pub market_slug: Option<String>,
    pub id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Token {
    pub token_id: String,
    pub outcome: Option<String>,
    pub price: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct OrderBook {
    pub asks: Vec<OrderBookEntry>,
}

#[derive(Debug, Deserialize)]
pub struct OrderBookEntry {
    pub price: String,
    pub size: String,
}

#[derive(Debug, Deserialize)]
struct OrderResponse {
    #[serde(rename = "orderID")]
    order_id: Option<String>,
    id: Option<String>,
    order: Option<OrderInner>,
}

#[derive(Debug, Deserialize)]
struct OrderInner {
    id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Position {
    pub asset_id: Option<String>,
    pub token_id: Option<String>,
    #[serde(rename = "tokenId")]
    pub token_id_camel: Option<String>,
    pub redeemable: Option<String>,
}

impl Position {
    pub fn effective_token_id(&self) -> Option<&str> {
        self.asset_id
            .as_deref()
            .or(self.token_id.as_deref())
            .or(self.token_id_camel.as_deref())
    }
}

// ── Client ──────────────────────────────────────────────────────────────────

pub struct PolymarketClient {
    http: reqwest::Client,
    api_key: String,
    api_secret: String,
    api_passphrase: String,
    _private_key: String,
    markets_cache: Vec<Market>,
    cache_ts: f64,
    active_orders: HashMap<String, String>, // token_id -> order_id
}

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

impl PolymarketClient {
    pub fn new(cfg: &crate::config::Config) -> Self {
        Self {
            http: reqwest::Client::new(),
            api_key: cfg.polymarket_api_key.clone(),
            api_secret: cfg.polymarket_api_secret.clone(),
            api_passphrase: cfg.polymarket_api_passphrase.clone(),
            _private_key: cfg.private_key.clone(),
            markets_cache: Vec::new(),
            cache_ts: 0.0,
            active_orders: HashMap::new(),
        }
    }

    fn auth_headers(&self) -> reqwest::header::HeaderMap {
        let mut headers = reqwest::header::HeaderMap::new();
        if let Ok(v) = reqwest::header::HeaderValue::from_str(&self.api_key) {
            headers.insert("POLY_API_KEY", v);
        }
        if let Ok(v) = reqwest::header::HeaderValue::from_str(&self.api_secret) {
            headers.insert("POLY_API_SECRET", v);
        }
        if let Ok(v) = reqwest::header::HeaderValue::from_str(&self.api_passphrase) {
            headers.insert("POLY_PASSPHRASE", v);
        }
        headers
    }

    // ── Market data ─────────────────────────────────────────────────────────

    pub async fn get_active_markets(&mut self, force_refresh: bool) -> &[Market] {
        let now = now_secs();
        if force_refresh || (now - self.cache_ts) > config::MARKET_REFRESH_SECONDS as f64 {
            self.refresh_markets().await;
        }
        &self.markets_cache
    }

    async fn refresh_markets(&mut self) {
        let url = format!("{}/markets", config::POLYMARKET_HOST);
        match self.http.get(&url).headers(self.auth_headers()).send().await {
            Ok(resp) => match resp.json::<serde_json::Value>().await {
                Ok(val) => {
                    let markets: Vec<Market> = if let Some(data) = val.get("data") {
                        serde_json::from_value(data.clone()).unwrap_or_default()
                    } else if val.is_array() {
                        serde_json::from_value(val).unwrap_or_default()
                    } else {
                        Vec::new()
                    };

                    self.markets_cache = markets
                        .into_iter()
                        .filter(|m| {
                            if m.active != Some(true) || m.closed == Some(true) {
                                return false;
                            }
                            let q = m.question.as_deref().unwrap_or("").to_lowercase();
                            if !config::TARGET_KEYWORDS.iter().any(|k| q.contains(k)) {
                                return false;
                            }
                            let vol: f64 = m
                                .volume
                                .as_deref()
                                .and_then(|v| v.parse().ok())
                                .unwrap_or(0.0);
                            vol >= config::MIN_MARKET_LIQUIDITY
                        })
                        .collect();
                    self.cache_ts = now_secs();
                    debug!("Market cache refreshed: {} active markets", self.markets_cache.len());
                }
                Err(e) => warn!("Market refresh parse error: {e}"),
            },
            Err(e) => warn!("Market refresh failed: {e}"),
        }
    }

    pub async fn get_orderbook(&self, token_id: &str) -> Option<OrderBook> {
        let url = format!("{}/book?token_id={token_id}", config::POLYMARKET_HOST);
        match self.http.get(&url).send().await {
            Ok(resp) => match resp.json::<OrderBook>().await {
                Ok(book) => Some(book),
                Err(e) => {
                    warn!("Orderbook parse failed ({}…): {e}", &token_id[..12.min(token_id.len())]);
                    None
                }
            },
            Err(e) => {
                warn!("Orderbook fetch failed ({}…): {e}", &token_id[..12.min(token_id.len())]);
                None
            }
        }
    }

    // ── Order management ────────────────────────────────────────────────────

    pub async fn place_order(&mut self, signal: &TradeSignal) -> Option<String> {
        // TODO: EIP-712 signing with alloy — for now, build and POST the order
        // The full signing implementation requires the exact Polymarket EIP-712
        // domain and type structure, which will be refined during integration testing.

        let order_body = serde_json::json!({
            "tokenID": signal.token_id,
            "price": format!("{:.4}", signal.price),
            "size": format!("{:.2}", signal.size_usdc),
            "side": "BUY",
            "orderType": "GTC",
            "feeRateBps": "0",
            "nonce": "0",
        });

        let url = format!("{}/order", config::POLYMARKET_HOST);
        match self
            .http
            .post(&url)
            .headers(self.auth_headers())
            .json(&order_body)
            .send()
            .await
        {
            Ok(resp) => match resp.json::<OrderResponse>().await {
                Ok(r) => {
                    let order_id = r
                        .order_id
                        .or(r.id)
                        .or_else(|| r.order.and_then(|o| o.id));

                    if let Some(ref id) = order_id {
                        self.active_orders.insert(signal.token_id.clone(), id.clone());
                        info!(
                            "[ORDER] {} {} | price={:.4} | size=${:.2} | id={id}",
                            signal.symbol,
                            signal.direction.to_uppercase(),
                            signal.price,
                            signal.size_usdc
                        );
                    } else {
                        warn!("Order placed but no ID returned");
                    }
                    order_id
                }
                Err(e) => {
                    warn!("Order response parse failed: {e}");
                    None
                }
            },
            Err(e) => {
                warn!("Order placement failed: {e}");
                None
            }
        }
    }

    pub async fn cancel_order(&self, order_id: &str) -> bool {
        let url = format!("{}/order/{order_id}", config::POLYMARKET_HOST);
        match self.http.delete(&url).headers(self.auth_headers()).send().await {
            Ok(_) => {
                info!("Order cancelled: {order_id}");
                true
            }
            Err(e) => {
                warn!("Cancel failed ({order_id}): {e}");
                false
            }
        }
    }

    pub async fn cancel_all(&mut self) {
        let orders: Vec<(String, String)> = self.active_orders.drain().collect();
        for (_token_id, order_id) in orders {
            self.cancel_order(&order_id).await;
        }
    }

    pub fn remove_order_tracking(&mut self, token_id: &str) {
        self.active_orders.remove(token_id);
    }

    // ── Account & Positions ─────────────────────────────────────────────────

    pub async fn get_positions(&self) -> Vec<Position> {
        let url = format!("{}/positions", config::POLYMARKET_HOST);
        match self.http.get(&url).headers(self.auth_headers()).send().await {
            Ok(resp) => resp.json::<Vec<Position>>().await.unwrap_or_default(),
            Err(e) => {
                warn!("Position fetch failed: {e}");
                Vec::new()
            }
        }
    }

    pub async fn get_resolved_position(&self, token_id: &str) -> Option<ResolvedPosition> {
        let url = format!("{}/positions/{token_id}", config::POLYMARKET_HOST);
        match self.http.get(&url).headers(self.auth_headers()).send().await {
            Ok(resp) => {
                if let Ok(pos) = resp.json::<Position>().await {
                    let redeemable: f64 = pos
                        .redeemable
                        .as_deref()
                        .and_then(|r| r.parse().ok())
                        .unwrap_or(0.0);
                    if redeemable > 0.0 {
                        return Some(ResolvedPosition {
                            payout: redeemable,
                        });
                    }
                }
                None
            }
            Err(e) => {
                debug!("Resolved position fetch: {e}");
                None
            }
        }
    }

    pub async fn get_balance(&self) -> Option<f64> {
        let url = format!("{}/balance", config::POLYMARKET_HOST);
        match self.http.get(&url).headers(self.auth_headers()).send().await {
            Ok(resp) => resp
                .text()
                .await
                .ok()
                .and_then(|t| t.trim().parse::<f64>().ok()),
            Err(e) => {
                warn!("Balance fetch failed: {e}");
                None
            }
        }
    }
}

#[derive(Debug)]
pub struct ResolvedPosition {
    pub payout: f64,
}
