# polymarket-arb v3 (Rust)

Latency arbitrage bot for Polymarket short-duration BTC/ETH/SOL up-down contracts.

Rewritten in Rust for lower latency and better concurrency. Uses Tokio async runtime
with independent tasks for each subsystem.

---

## Architecture

```
BinanceFeed (WebSocket, <50ms)
    │  (timestamp, price, volume) per tick
    ▼
EdgeDetector::scan_market()
    │  1. VWAP momentum over last 15s
    │  2. sigmoid → P(win)
    │  3. compare to Polymarket odds
    │  4. fetch orderbook → limit price at 3rd ask level
    │  5. verify 2× depth available
    ▼
RiskManager::kelly_size()
    │  fractional Kelly (25%), capped at 8% portfolio
    ▼
PolymarketClient::place_order()  ← GTC limit at orderbook price
    │
    ├──→ Watchdog (every 2s): cancel if unfilled after 8s
    │
    └──→ SettlementMonitor (every 30s): detect resolution → record_close()
```

---

## Build & Run

```bash
# Build
cargo build --release

# Configure
cp .env.example .env
# Edit .env with your keys

# Run
./target/release/polymarket-arb
```

### Required environment variables

```
PRIVATE_KEY=0x...
POLYMARKET_API_KEY=...
POLYMARKET_API_SECRET=...
POLYMARKET_API_PASSPHRASE=...
POLYGON_RPC=https://polygon-rpc.com    # optional
STARTING_CAPITAL=100                    # optional, default 100
```

---

## AWS Setup

### 1. Launch instance

- **AMI**: Ubuntu 24.04 LTS
- **Instance type**: t3.small ($15.18/month)
- **Region**: us-east-1 (N. Virginia) — mandatory for latency
- **Storage**: 8GB gp3

### 2. Server setup

```bash
sudo apt update && sudo apt upgrade -y
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source ~/.cargo/env
sudo apt install -y build-essential pkg-config libssl-dev git tmux
```

### 3. Deploy

```bash
git clone <this-repo>
cd polymarket-arb
cargo build --release
cp .env.example .env
nano .env
```

### 4. Run with systemd

```ini
[Unit]
Description=Polymarket Arbitrage Bot
After=network.target

[Service]
User=bot
WorkingDirectory=/home/bot/polymarket-arb
ExecStart=/home/bot/polymarket-arb/target/release/polymarket-arb
Restart=on-failure
RestartSec=10
StandardOutput=append:/home/bot/polymarket-arb/bot.log
StandardError=append:/home/bot/polymarket-arb/bot.log

[Install]
WantedBy=multi-user.target
```

---

## Why VWAP matters

Raw momentum: `(price_end - price_start) / price_start`

A 0.4% move on 30k USD of Binance volume could be one retail market order.
A 0.4% move on 3M USD of volume is institutional flow — a real directional signal.

VWAP momentum volume-weights the ticks so low-volume noise trades get
proportionally less influence.

## Why orderbook limit price matters

Static offset (`market_price + 0.02`) overpays in liquid markets and underpays
in thin ones. Orderbook depth approach: walk to 3rd ask level, verify 2× position
size available, use that price. Thin markets get filtered out automatically.

## Tuning after first week

Look at `trades.csv` after 7 days:

1. **Watchdog firing often?** (>2/day) → tighten `MIN_MARKET_LIQUIDITY`
2. **Win rate below 58%?** → raise `MIN_EDGE_THRESHOLD`
3. **Large edge trades losing?** → adjust `SIGMOID_SENSITIVITY`
4. **BTC vs ETH vs SOL?** → compare per-symbol win rates, remove weak ones from `TARGET_KEYWORDS`
