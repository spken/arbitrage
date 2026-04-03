use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub private_key: String,
    pub polymarket_api_key: String,
    pub polymarket_api_secret: String,
    pub polymarket_api_passphrase: String,
    #[serde(default = "default_starting_capital")]
    pub starting_capital: f64,
}

fn default_starting_capital() -> f64 {
    100.0
}

// ── Capital & Risk ──────────────────────────────────────────────────────────
pub const KELLY_FRACTION: f64 = 0.25;
pub const MAX_POSITION_PCT: f64 = 0.08;
pub const DAILY_LOSS_LIMIT_PCT: f64 = 0.20;
pub const TOTAL_DRAWDOWN_KILL_PCT: f64 = 0.40;
pub const MIN_MARKET_LIQUIDITY: f64 = 50_000.0;

// ── Edge Detection ──────────────────────────────────────────────────────────
pub const MIN_EDGE_THRESHOLD: f64 = 0.05;
pub const MOMENTUM_WINDOW_SECONDS: f64 = 15.0;
pub const MOMENTUM_THRESHOLD: f64 = 0.003;
pub const SIGMOID_SENSITIVITY: f64 = 15.0;
pub const MIN_VOLUME_TICKS: usize = 5;

// ── Order Management ────────────────────────────────────────────────────────
pub const ORDER_CANCEL_TIMEOUT_SECS: f64 = 8.0;
pub const ORDERBOOK_DEPTH_LEVEL: usize = 3;
pub const ORDERBOOK_MIN_DEPTH: f64 = 2.0;

// ── Polymarket ──────────────────────────────────────────────────────────────
pub const POLYMARKET_HOST: &str = "https://clob.polymarket.com";
pub const MARKET_REFRESH_SECONDS: u64 = 60;

pub const TARGET_KEYWORDS: &[&str] = &[
    "higher", "lower", "above", "below",
    "btc", "eth", "bitcoin", "ethereum",
    "sol", "solana",
];

// ── Settlement Monitor ──────────────────────────────────────────────────────
pub const SETTLEMENT_POLL_SECONDS: u64 = 30;

// ── Logging ─────────────────────────────────────────────────────────────────
pub const LOG_FILE: &str = "bot.log";
pub const TRADE_LOG_FILE: &str = "trades.csv";

impl Config {
    pub fn load() -> Result<Self, envy::Error> {
        dotenvy::dotenv().ok();
        envy::from_env()
    }
}
