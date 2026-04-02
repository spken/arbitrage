import os
from dotenv import load_dotenv

_ = load_dotenv()

# ── Wallet & API ──────────────────────────────────────────────────────────────
PRIVATE_KEY: str               = os.getenv("PRIVATE_KEY", "")
POLYMARKET_API_KEY: str        = os.getenv("POLYMARKET_API_KEY", "")
POLYMARKET_API_SECRET: str     = os.getenv("POLYMARKET_API_SECRET", "")
POLYMARKET_API_PASSPHRASE: str = os.getenv("POLYMARKET_API_PASSPHRASE", "")
POLYGON_RPC: str               = os.getenv("POLYGON_RPC", "https://polygon-rpc.com")

# ── Capital & Risk ────────────────────────────────────────────────────────────
STARTING_CAPITAL: float        = float(os.getenv("STARTING_CAPITAL", "100"))
KELLY_FRACTION: float          = 0.25
MAX_POSITION_PCT: float        = 0.08
DAILY_LOSS_LIMIT_PCT: float    = 0.20
TOTAL_DRAWDOWN_KILL_PCT: float = 0.40
MIN_MARKET_LIQUIDITY: float    = 50_000

# ── Edge Detection ────────────────────────────────────────────────────────────
MIN_EDGE_THRESHOLD: float      = 0.05
MOMENTUM_WINDOW_SECONDS: int   = 15
MOMENTUM_THRESHOLD: float      = 0.003
SIGMOID_SENSITIVITY: float     = 15.0
MIN_VOLUME_TICKS: int          = 5

# ── Order Management ─────────────────────────────────────────────────────────
ORDER_CANCEL_TIMEOUT: float    = 8.0
ORDERBOOK_DEPTH_LEVEL: int     = 3
ORDERBOOK_MIN_DEPTH: float     = 2.0

# ── Polymarket ────────────────────────────────────────────────────────────────
POLYMARKET_HOST: str           = "https://clob.polymarket.com"
CHAIN_ID: int                  = 137
MARKET_REFRESH_SECONDS: int    = 60

TARGET_KEYWORDS: list[str] = [
    "higher", "lower", "above", "below",
    "btc", "eth", "bitcoin", "ethereum",
    "sol", "solana",
]

# ── Settlement Monitor ────────────────────────────────────────────────────────
SETTLEMENT_POLL_SECONDS: int   = 30

# ── Logging ───────────────────────────────────────────────────────────────────
LOG_FILE: str        = "bot.log"
TRADE_LOG_FILE: str  = "trades.csv"
