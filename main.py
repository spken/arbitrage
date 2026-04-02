"""
main.py — entry point

Run with:
    python main.py

Env vars are loaded from .env automatically.
Copy .env.example to .env and fill in your credentials before running.
"""

import asyncio
import logging
import signal
import sys
from types import FrameType

import config
from trader import Trader


# ── Logging setup ─────────────────────────────────────────────────────────────

def setup_logging() -> None:
    fmt = "%(asctime)s | %(levelname)-8s | %(message)s"
    logging.basicConfig(
        level    = logging.INFO,
        format   = fmt,
        handlers = [
            logging.StreamHandler(sys.stdout),
            logging.FileHandler(config.LOG_FILE),
        ],
    )
    # Suppress verbose library noise
    logging.getLogger("websockets").setLevel(logging.WARNING)
    logging.getLogger("urllib3").setLevel(logging.WARNING)
    logging.getLogger("web3").setLevel(logging.WARNING)


# ── Config validation ─────────────────────────────────────────────────────────

def validate_config() -> None:
    required = {
        "PRIVATE_KEY":             config.PRIVATE_KEY,
        "POLYMARKET_API_KEY":      config.POLYMARKET_API_KEY,
        "POLYMARKET_API_SECRET":   config.POLYMARKET_API_SECRET,
        "POLYMARKET_API_PASSPHRASE": config.POLYMARKET_API_PASSPHRASE,
    }
    missing = [k for k, v in required.items() if not v]
    if missing:
        print(f"\n❌  Missing credentials: {', '.join(missing)}")
        print("    Copy .env.example to .env and fill in your credentials.\n")
        sys.exit(1)

    if config.STARTING_CAPITAL <= 0:
        print("❌  STARTING_CAPITAL must be > 0")
        sys.exit(1)

    print(f"✅  Config valid | Starting capital: ${config.STARTING_CAPITAL:.2f}")


# ── Main ──────────────────────────────────────────────────────────────────────

async def main() -> None:
    setup_logging()
    validate_config()

    bot  = Trader()
    loop = asyncio.get_event_loop()

    def on_signal(sig: int, _frame: FrameType | None) -> None:
        print(f"\nReceived signal {sig}. Shutting down…")
        bot.stop()
        # Give tasks a moment to clean up, then stop the loop
        loop.call_later(2, loop.stop)  # pyright: ignore[reportUnusedCallResult]

    _ = signal.signal(signal.SIGINT,  on_signal)
    _ = signal.signal(signal.SIGTERM, on_signal)

    await bot.start()


if __name__ == "__main__":
    asyncio.run(main())
