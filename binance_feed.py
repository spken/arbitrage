"""
binance_feed.py

Persistent Binance WebSocket — now stores (timestamp, price, volume) per tick.
Volume is used for VWAP-based momentum in EdgeDetector.

Binance @trade stream fields used:
  p  = price
  q  = quantity (volume of this trade in base asset)
  T  = trade time (ms)
"""

import asyncio
import json
import logging
from collections import deque
from collections.abc import Awaitable, Callable
from dataclasses import dataclass

import websockets

logger = logging.getLogger(__name__)

BINANCE_WS  = "wss://stream.binance.com:9443/stream"
SYMBOLS     = ["btcusdt", "ethusdt", "solusdt"]
BUFFER_SIZE = 600


@dataclass(slots=True)
class Tick:
    timestamp: float
    price: float
    volume: float


class BinanceFeed:

    def __init__(self) -> None:
        self._buffers: dict[str, deque[Tick]] = {
            s.upper(): deque(maxlen=BUFFER_SIZE) for s in SYMBOLS
        }
        self._callbacks: list[Callable[[str, float, float, float], Awaitable[None]]] = []
        self._running: bool = False

    # ── Public API ────────────────────────────────────────────────────────────

    def register_callback(self, fn: Callable[[str, float, float, float], Awaitable[None]]) -> None:
        self._callbacks.append(fn)

    def get_price(self, symbol: str) -> float | None:
        buf = self._buffers.get(symbol.upper())
        return buf[-1].price if buf else None

    def get_ticks_since(self, symbol: str, since_ts: float) -> list[Tick]:
        """Returns list of Tick objects since since_ts."""
        buf = self._buffers.get(symbol.upper(), deque())
        return [t for t in buf if t.timestamp >= since_ts]

    def has_data(self, symbol: str, min_ticks: int = 10) -> bool:
        return len(self._buffers.get(symbol.upper(), deque())) >= min_ticks

    # ── WebSocket lifecycle ──────────────────────────────────────────────────

    async def run(self) -> None:
        streams = "/".join(f"{s}@trade" for s in SYMBOLS)
        url = f"{BINANCE_WS}?streams={streams}"
        self._running = True

        while self._running:
            try:
                async with websockets.connect(
                    url, ping_interval=20, ping_timeout=10, close_timeout=5
                ) as ws:
                    logger.info("Binance WebSocket connected")
                    async for raw in ws:
                        if isinstance(raw, bytes):
                            raw = raw.decode()
                        await self._handle(raw)
            except websockets.ConnectionClosed as e:
                logger.warning(f"Binance WS closed: {e}. Reconnecting in 2s…")
                await asyncio.sleep(2)
            except OSError as e:
                logger.warning(f"Binance WS network error: {e}. Reconnecting in 3s…")
                await asyncio.sleep(3)
            except Exception as e:
                logger.error(f"Binance WS unexpected error: {e}. Reconnecting in 5s…")
                await asyncio.sleep(5)

    def stop(self) -> None:
        self._running = False

    # ── Internal ──────────────────────────────────────────────────────────────

    async def _handle(self, raw: str) -> None:
        try:
            msg  = json.loads(raw)
            data = msg.get("data", {})
            if data.get("e") != "trade":
                return

            symbol: str = data["s"]
            price  = float(data["p"])
            volume = float(data["q"])          # trade quantity
            ts     = float(data["T"]) / 1000.0

            self._buffers[symbol].append(Tick(timestamp=ts, price=price, volume=volume))

            for cb in self._callbacks:
                try:
                    await cb(symbol, price, volume, ts)
                except Exception as e:
                    logger.debug(f"Callback error: {e}")

        except (KeyError, ValueError, json.JSONDecodeError) as e:
            logger.debug(f"Feed parse error: {e}")
