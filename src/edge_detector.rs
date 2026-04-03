use tracing::{debug, info};

use crate::binance_feed::{self, TickBuffers};
use crate::config;
use crate::polymarket_client::{Market, PolymarketClient};
use crate::risk_manager::RiskManager;
use crate::types::TradeSignal;

pub struct EdgeDetector;

impl EdgeDetector {
    // ── VWAP Momentum ───────────────────────────────────────────────────────

    /// Returns (momentum_pct, total_volume) or None if insufficient data.
    pub async fn get_vwap_momentum(
        buffers: &TickBuffers,
        symbol: &str,
    ) -> Option<(f64, f64)> {
        let since = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64()
            - config::MOMENTUM_WINDOW_SECONDS;

        let feed_symbol = format!("{symbol}USDT");
        let ticks = binance_feed::get_ticks_since(buffers, &feed_symbol, since).await;

        let n = config::MIN_VOLUME_TICKS;
        if ticks.len() < n * 2 {
            return None;
        }

        // VWAP of first N ticks
        let early = &ticks[..n];
        let vol_early: f64 = early.iter().map(|t| t.volume).sum();
        if vol_early == 0.0 {
            return None;
        }
        let vwap_start: f64 = early.iter().map(|t| t.price * t.volume).sum::<f64>() / vol_early;

        // VWAP of last N ticks
        let recent = &ticks[ticks.len() - n..];
        let vol_recent: f64 = recent.iter().map(|t| t.volume).sum();
        if vol_recent == 0.0 {
            return None;
        }
        let vwap_end: f64 = recent.iter().map(|t| t.price * t.volume).sum::<f64>() / vol_recent;

        let total_volume: f64 = ticks.iter().map(|t| t.volume).sum();
        let momentum = (vwap_end - vwap_start) / vwap_start;

        Some((momentum, total_volume))
    }

    /// Sigmoid maps VWAP momentum magnitude to win probability.
    pub fn momentum_to_probability(momentum: f64, is_higher_market: bool) -> f64 {
        let k = config::SIGMOID_SENSITIVITY;
        let raw = 1.0 / (1.0 + (-k * momentum.abs()).exp());

        if is_higher_market {
            if momentum > 0.0 { raw } else { 1.0 - raw }
        } else if momentum < 0.0 {
            raw
        } else {
            1.0 - raw
        }
    }

    // ── Limit Price from Orderbook ──────────────────────────────────────────

    /// Fetches orderbook and sets limit price at Nth ask level.
    /// Returns None if book is too thin.
    pub async fn get_limit_price(
        poly: &PolymarketClient,
        token_id: &str,
        required_size: f64,
    ) -> Option<f64> {
        let book = poly.get_orderbook(token_id).await?;

        let mut asks: Vec<_> = book.asks;
        asks.sort_by(|a, b| {
            a.price
                .parse::<f64>()
                .unwrap_or(f64::MAX)
                .partial_cmp(&b.price.parse::<f64>().unwrap_or(f64::MAX))
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        if asks.len() < config::ORDERBOOK_DEPTH_LEVEL {
            return None;
        }

        // Verify sufficient depth
        let cumulative_size: f64 = asks[..config::ORDERBOOK_DEPTH_LEVEL]
            .iter()
            .filter_map(|a| a.size.parse::<f64>().ok())
            .sum();

        let min_required = required_size * config::ORDERBOOK_MIN_DEPTH;
        if cumulative_size < min_required {
            debug!(
                "Book too thin: {cumulative_size:.1} USDC available, need {min_required:.1}"
            );
            return None;
        }

        let target_price: f64 = asks[config::ORDERBOOK_DEPTH_LEVEL - 1]
            .price
            .parse()
            .ok()?;

        Some((target_price * 10000.0).round() / 10000.0_f64.min(0.96))
    }

    // ── Market Scanning ─────────────────────────────────────────────────────

    pub async fn scan_market(
        market: &Market,
        buffers: &TickBuffers,
        poly: &PolymarketClient,
        risk: &mut RiskManager,
    ) -> Option<TradeSignal> {
        let question = market.question.as_deref().unwrap_or("").to_lowercase();

        // Identify asset
        let symbol = if question.contains("btc") || question.contains("bitcoin") {
            "BTC"
        } else if question.contains("eth") || question.contains("ethereum") {
            "ETH"
        } else if question.contains("sol") || question.contains("solana") {
            "SOL"
        } else {
            return None;
        };

        let is_higher_market = ["higher", "above", "over", "up"]
            .iter()
            .any(|w| question.contains(w));

        let feed_symbol = format!("{symbol}USDT");
        if !binance_feed::has_data(buffers, &feed_symbol, config::MIN_VOLUME_TICKS * 2).await {
            return None;
        }

        let (momentum, total_volume) = Self::get_vwap_momentum(buffers, symbol).await?;

        if momentum.abs() < config::MOMENTUM_THRESHOLD {
            return None;
        }

        let tokens = market.tokens.as_ref()?;
        if tokens.len() < 2 {
            return None;
        }

        let yes_token = tokens
            .iter()
            .find(|t| t.outcome.as_deref().map(|o| o.to_lowercase()) == Some("yes".into()))
            .unwrap_or(&tokens[0]);
        let no_token = tokens
            .iter()
            .find(|t| t.outcome.as_deref().map(|o| o.to_lowercase()) == Some("no".into()))
            .unwrap_or(&tokens[1]);

        let p_market_yes: f64 = yes_token.price.as_deref()?.parse().ok()?;
        let p_market_no: f64 = no_token.price.as_deref()?.parse().ok()?;

        if !(0.02..=0.98).contains(&p_market_yes) {
            return None;
        }

        let p_model_yes = Self::momentum_to_probability(momentum, is_higher_market);

        let edge_yes = p_model_yes - p_market_yes;
        let edge_no = (1.0 - p_model_yes) - p_market_no;

        if edge_yes.max(edge_no) < config::MIN_EDGE_THRESHOLD {
            return None;
        }

        // Choose direction
        let (direction, token_id, p_market, p_model, best_edge) = if edge_yes >= edge_no {
            ("yes", &yes_token.token_id, p_market_yes, p_model_yes, edge_yes)
        } else {
            (
                "no",
                &no_token.token_id,
                p_market_no,
                1.0 - p_model_yes,
                edge_no,
            )
        };

        // Preliminary Kelly size (rough estimate for depth check)
        let rough_price = p_market + 0.02;
        let prelim_size = risk.kelly_size(p_model, rough_price);
        if prelim_size < 1.0 {
            return None;
        }

        // Orderbook-based limit price
        let limit_price = Self::get_limit_price(poly, token_id, prelim_size).await?;

        // Final Kelly size with accurate limit price
        let size = risk.kelly_size(p_model, limit_price);
        if size < 1.0 {
            return None;
        }

        info!(
            "[EDGE] {symbol} {} | vwap_momentum={momentum:+.4} | vol={total_volume:.2} | \
             p_model={p_model:.3} | p_market={p_market:.3} | \
             edge={best_edge:.3} | limit={limit_price:.4} | size=${size:.2}",
            direction.to_uppercase()
        );

        let market_id = market
            .market_slug
            .clone()
            .or_else(|| market.id.clone())
            .unwrap_or_default();

        Some(TradeSignal {
            market_id,
            condition_id: market.condition_id.clone().unwrap_or_default(),
            token_id: token_id.clone(),
            direction: direction.to_string(),
            price: limit_price,
            size_usdc: size,
            edge: best_edge,
            p_model,
            p_market,
            symbol: symbol.to_string(),
            momentum,
        })
    }
}
