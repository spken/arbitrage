# polymarket-arb v2

Latency arbitrage bot for Polymarket short-duration BTC/ETH/SOL up-down contracts.

## What's new in v2

| Component | Change |
|-----------|--------|
| `binance_feed.py` | Stores volume per tick (timestamp, price, **volume**) |
| `edge_detector.py` | VWAP momentum instead of raw price change; orderbook-based limit price |
| `polymarket_client.py` | Added `get_resolved_position()` for settlement |
| `settlement_monitor.py` | **New** — polls for resolved positions, closes P&L in RiskManager |
| `trader.py` | **New** cancellation watchdog; settlement monitor integrated as background task |
| `config.py` | New constants: `ORDER_CANCEL_TIMEOUT`, `ORDERBOOK_DEPTH_LEVEL`, `ORDERBOOK_MIN_DEPTH`, `SETTLEMENT_POLL_SECONDS` |

---

## Why AWS us-east-1, not DigitalOcean

Polymarket's CLOB API runs on AWS. AWS us-east-1 (N. Virginia) is the same
availability zone. Round-trip API latency: **1–3ms**.

DigitalOcean NYC1 is geographically close but crosses datacenter boundaries:
**8–15ms** round-trip. On a 2–3 second arbitrage window, that 12ms difference
is meaningful at scale.

AWS t3.small us-east-1 = **$15.18/month**. Cheaper than DigitalOcean Premium
($18/mo) and better latency. No reason to start anywhere else.

---

## Architecture

```
BinanceFeed (WebSocket, <50ms)
    │  (timestamp, price, volume) per tick
    ▼
EdgeDetector.scan_market()
    │  1. VWAP momentum over last 15s
    │  2. sigmoid → P(win)
    │  3. compare to Polymarket odds
    │  4. fetch orderbook → limit price at 3rd ask level
    │  5. verify 2× depth available
    ▼
RiskManager.kelly_size()
    │  fractional Kelly (25%), capped at 8% portfolio
    ▼
PolymarketClient.place_order()  ← GTC limit at orderbook price
    │
    ├──→ Watchdog (every 2s): cancel if unfilled after 8s
    │
    └──→ SettlementMonitor (every 30s): detect resolution → record_close()
```

---

## Why VWAP matters

Raw momentum: `(price_end - price_start) / price_start`

A 0.4% move on 30k USD of Binance volume could be one retail market order.
A 0.4% move on 3M USD of volume is institutional flow — a real directional signal.

VWAP momentum:
```python
vwap_start = sum(price * volume for first N ticks) / sum(volume for first N ticks)
vwap_end   = sum(price * volume for last N ticks)  / sum(volume for last N ticks)
momentum   = (vwap_end - vwap_start) / vwap_start
```

Same formula structure, volume-weighted. Low-volume noise trades get proportionally
less influence. High-volume institutional trades dominate. Better signal-to-noise ratio.

## Why orderbook limit price matters

Static offset (`market_price + 0.02`):
- Liquid market: overpays by up to 1.5 cents per share
- Thin market: might not cover the real ask spread, order never fills

Orderbook depth approach:
1. Fetch live order book
2. Walk to the 3rd ask price level
3. Verify at least 2× position size available at that level
4. Use that price as limit — tight in liquid markets, realistic in thin ones

Side effect: thin markets where the 3rd level is $0.08 away get filtered out
automatically. You don't place orders on markets you can't exit cleanly.

## Why the cancellation watchdog matters

A GTC order at a fair price on a liquid market fills in under 2 seconds.
If it hasn't filled in 8 seconds, one of three things happened:

1. The arbitrage window closed before our order hit the book
2. The market moved against us while the order was in flight
3. A network issue delayed our order submission

In all three cases, the right move is to cancel. The signal that triggered the
trade is no longer valid. Holding a stale limit order means it might fill hours
later at a price that no longer has any edge. The watchdog prevents this.

Seeing the watchdog fire frequently (more than 1-2 times per day) is a signal
that something is wrong: your limit price may be too conservative, or the market
liquidity filter needs to be tightened.

## Why the settlement monitor matters

Without it, the RiskManager doesn't know when positions resolve. It tracks capital
as "committed" indefinitely, which means:
- Kelly sizing underestimates available capital
- P&L reporting is inaccurate
- The drawdown kill switch triggers based on incomplete data

The monitor polls every 30 seconds, compares your tracked positions against what
Polymarket reports as open, and calls `record_close()` for anything that resolved.
P&L is calculated from Polymarket's `redeemable` field — the actual USDC you receive.

---

## AWS Setup

### 1. Launch instance

- **AMI**: Ubuntu 24.04 LTS (search "ubuntu 24" in community AMIs)
- **Instance type**: t3.small ($15.18/month)
- **Region**: us-east-1 (N. Virginia) — mandatory for latency
- **Storage**: 8GB gp3 (default)
- **Security group**: allow SSH (port 22) from your IP only
- **Key pair**: create and download .pem file

```bash
ssh -i your-key.pem ubuntu@YOUR_EC2_IP
```

### 2. Server setup

```bash
sudo apt update && sudo apt upgrade -y
sudo apt install -y python3.12 python3.12-venv python3-pip git tmux

# Create bot user
sudo adduser bot
sudo usermod -aG sudo bot
su - bot
```

### 3. Deploy

```bash
git clone <this-repo>
cd polymarket-arb
python3.12 -m venv venv
source venv/bin/activate
pip install -r requirements.txt
cp .env.example .env
nano .env
```

### 4. Run with systemd (survives reboots and crashes)

```bash
sudo nano /etc/systemd/system/polybot.service
```

```ini
[Unit]
Description=Polymarket Arbitrage Bot
After=network.target

[Service]
User=bot
WorkingDirectory=/home/bot/polymarket-arb
ExecStart=/home/bot/polymarket-arb/venv/bin/python main.py
Restart=on-failure
RestartSec=10
StandardOutput=append:/home/bot/polymarket-arb/bot.log
StandardError=append:/home/bot/polymarket-arb/bot.log

[Install]
WantedBy=multi-user.target
```

```bash
sudo systemctl daemon-reload
sudo systemctl enable polybot
sudo systemctl start polybot

# Check status
sudo systemctl status polybot
tail -f /home/bot/polymarket-arb/bot.log
```

---

## Monthly cost: $100 starting capital

| Item | Cost |
|------|------|
| AWS EC2 t3.small us-east-1 | $15.18 |
| Alchemy Polygon RPC (free tier) | $0 |
| Polygon gas (~200 trades × ~$0.02) | ~$4 |
| **Total** | **~$19/month** |

At $100 capital, Kelly-sized trades will be roughly $3–8 each.
Infrastructure cost of $19/month means you need ~19% monthly return just to break even.
This is an experiment — the goal is to validate the edge and system reliability,
not to profit significantly at this capital level.

When/if you scale to $1,000+, the infrastructure cost becomes negligible
and actual returns are meaningful.

---

## Credential setup

**Private key**: MetaMask → Settings → Security & Privacy → Export Private Key. Prefix with `0x`.

**Polymarket API**: https://docs.polymarket.com/#authentication
Sign a message with your wallet to generate key/secret/passphrase.
Takes about 2 minutes.

**Alchemy RPC**: https://www.alchemy.com → New App → Polygon Mainnet → copy HTTPS URL.
Free tier = 300M compute units/month, sufficient for this bot.

---

## Tuning after first week

Look at `trades.csv` after 7 days. Key questions:

1. **Is the watchdog firing often?** If >2 cancellations/day: your limit price offset may be too tight, or you're hitting thin markets. Tighten `MIN_MARKET_LIQUIDITY`.
2. **What's your actual win rate?** Below 58% consistently: `MIN_EDGE_THRESHOLD` may need to go up (trade less, but higher quality).
3. **What's the average edge on winning vs losing trades?** Large edge trades losing = your momentum→probability calibration needs adjustment (`SIGMOID_SENSITIVITY`).
4. **BTC vs ETH vs SOL?** Compare win rates per symbol. If SOL signals are weaker, remove it from `TARGET_KEYWORDS` for now.
