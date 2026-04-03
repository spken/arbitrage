use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::Write;

use chrono::Local;
use tracing::{debug, error, info, warn};

use crate::config;
use crate::types::TradeSignal;

pub struct RiskManager {
    pub starting_capital: f64,
    pub portfolio_value: f64,
    pub daily_start_value: f64,
    today: chrono::NaiveDate,
    killed: bool,
    halted_daily: bool,
    open_positions: HashMap<String, f64>, // token_id -> USDC committed
}

impl RiskManager {
    pub fn new(starting_capital: f64) -> Self {
        let mut rm = Self {
            starting_capital,
            portfolio_value: starting_capital,
            daily_start_value: starting_capital,
            today: Local::now().date_naive(),
            killed: false,
            halted_daily: false,
            open_positions: HashMap::new(),
        };
        rm.init_trade_log();
        rm
    }

    // ── State ───────────────────────────────────────────────────────────────

    pub fn is_live(&self) -> bool {
        !self.killed && !self.halted_daily
    }

    // ── Daily reset ─────────────────────────────────────────────────────────

    fn check_daily_reset(&mut self) {
        let today = Local::now().date_naive();
        if today != self.today {
            info!(
                "New day. Resetting daily PnL. Portfolio: ${:.2}",
                self.portfolio_value
            );
            self.daily_start_value = self.portfolio_value;
            self.today = today;
            self.halted_daily = false;
        }
    }

    // ── Limit checks ────────────────────────────────────────────────────────

    pub fn check_limits(&mut self) -> bool {
        self.check_daily_reset();

        if self.killed {
            error!("KILL SWITCH ACTIVE — bot is permanently stopped.");
            return false;
        }

        // Total drawdown
        let total_dd =
            (self.starting_capital - self.portfolio_value) / self.starting_capital;
        if total_dd >= config::TOTAL_DRAWDOWN_KILL_PCT {
            self.killed = true;
            error!(
                "KILL SWITCH TRIGGERED | Total drawdown: {:.1}% | Portfolio: ${:.2} | Bot stopped permanently.",
                total_dd * 100.0,
                self.portfolio_value
            );
            return false;
        }

        // Daily loss
        if self.daily_start_value > 0.0 {
            let daily_dd =
                (self.daily_start_value - self.portfolio_value) / self.daily_start_value;
            if daily_dd >= config::DAILY_LOSS_LIMIT_PCT {
                self.halted_daily = true;
                warn!(
                    "DAILY HALT | Daily loss: {:.1}% | Resuming at next calendar day.",
                    daily_dd * 100.0
                );
                return false;
            }
        }

        true
    }

    // ── Kelly sizing ────────────────────────────────────────────────────────

    pub fn kelly_size(&self, win_prob: f64, entry_price: f64) -> f64 {
        if !(0.01 < entry_price && entry_price < 0.99) {
            return 0.0;
        }

        let b = (1.0 / entry_price) - 1.0; // net odds
        let q = 1.0 - win_prob;

        if b <= 0.0 {
            return 0.0;
        }
        let kelly_full = (b * win_prob - q) / b;

        if kelly_full <= 0.0 {
            return 0.0;
        }

        let fraction = kelly_full * config::KELLY_FRACTION;
        let max_position = self.portfolio_value * config::MAX_POSITION_PCT;
        let size = (fraction * self.portfolio_value).min(max_position);

        (size.max(0.0) * 100.0).round() / 100.0
    }

    // ── Position tracking ───────────────────────────────────────────────────

    pub fn record_open(&mut self, token_id: &str, size_usdc: f64) {
        *self.open_positions.entry(token_id.to_string()).or_insert(0.0) += size_usdc;
        debug!("Position opened: {}… ${:.2}", &token_id[..12.min(token_id.len())], size_usdc);
    }

    pub fn record_close(
        &mut self,
        token_id: &str,
        pnl: f64,
        outcome: &str,
        signal: Option<&TradeSignal>,
    ) {
        self.open_positions.remove(token_id);
        self.portfolio_value = ((self.portfolio_value + pnl) * 10000.0).round() / 10000.0;

        let emoji = if pnl > 0.0 { "+" } else { "-" };
        info!(
            "{emoji} Closed | outcome={outcome} | pnl=${pnl:+.2} | portfolio=${:.2}",
            self.portfolio_value
        );

        if let Some(sig) = signal {
            self.log_trade(sig, pnl, outcome);
        }

        self.check_limits();
    }

    // ── Reporting ───────────────────────────────────────────────────────────

    pub fn summary(&self) -> String {
        let total_ret =
            (self.portfolio_value - self.starting_capital) / self.starting_capital;
        let daily_ret =
            (self.portfolio_value - self.daily_start_value) / self.daily_start_value;
        format!(
            "Portfolio: ${:.2} | Total: {:+.2}% | Daily: {:+.2}% | Open: {} positions",
            self.portfolio_value,
            total_ret * 100.0,
            daily_ret * 100.0,
            self.open_positions.len()
        )
    }

    // ── CSV trade log ───────────────────────────────────────────────────────

    fn init_trade_log(&mut self) {
        if let Ok(mut f) = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(config::TRADE_LOG_FILE)
        {
            let _ = writeln!(
                f,
                "timestamp,market_id,direction,symbol,price,size_usdc,edge,p_model,p_market,momentum,pnl,portfolio_value,outcome"
            );
        }
    }

    fn log_trade(&self, signal: &TradeSignal, pnl: f64, outcome: &str) {
        if let Ok(mut f) = OpenOptions::new()
            .append(true)
            .open(config::TRADE_LOG_FILE)
        {
            let ts = Local::now().format("%Y-%m-%d %H:%M:%S");
            let _ = writeln!(
                f,
                "{ts},{},{},{},{:.4},{:.4},{:.4},{:.4},{:.4},{:.6},{:.4},{:.4},{outcome}",
                signal.market_id,
                signal.direction,
                signal.symbol,
                signal.price,
                signal.size_usdc,
                signal.edge,
                signal.p_model,
                signal.p_market,
                signal.momentum,
                pnl,
                self.portfolio_value,
            );
        }
    }
}
