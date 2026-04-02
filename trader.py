"""
trader.py

Main orchestrator. v2 adds:
  - Cancellation watchdog: cancels unfilled orders after ORDER_CANCEL_TIMEOUT seconds
  - Settlement monitor integration: closed positions flow back to RiskManager
  - EdgeDetector now receives polymarket_client (needed for orderbook depth check)
  - order_timestamps dict tracks when each order was placed
"""

import asyncio
import logging
import time
from dataclasses import dataclass

import config
from binance_feed import BinanceFeed
from edge_detector import EdgeDetector, TradeSignal
from polymarket_client import PolymarketClient
from risk_manager import RiskManager
from settlement_monitor import SettlementMonitor

logger = logging.getLogger(__name__)

SCAN_INTERVAL    = 0.5
COOLDOWN_SECONDS = 60.0
WARMUP_SECONDS   = 10
WATCHDOG_INTERVAL = 2.0  # check for stale orders every 2 seconds


@dataclass
class TrackedOrder:
    order_id: str
    placed_at: float
    signal: TradeSignal


class Trader:

    def __init__(self) -> None:
        self.feed: BinanceFeed       = BinanceFeed()
        self.poly: PolymarketClient  = PolymarketClient()
        self.risk: RiskManager       = RiskManager(config.STARTING_CAPITAL)
        self.edge: EdgeDetector      = EdgeDetector(self.feed, self.poly)
        self.settlement: SettlementMonitor = SettlementMonitor(self.poly, self.risk)

        self._running: bool   = False
        self._last_scan: float = 0
        self._cooldowns: dict[str, float] = {}

        # token_id → TrackedOrder
        # Used by watchdog to cancel stale orders
        self._order_timestamps: dict[str, TrackedOrder] = {}

    # ── Startup ───────────────────────────────────────────────────────────────

    async def start(self) -> None:
        self._running = True
        logger.info("=" * 60)
        logger.info(f"Bot starting | Capital: ${config.STARTING_CAPITAL:.2f}")
        logger.info(
            f"Risk: max_pos={config.MAX_POSITION_PCT:.0%} | "
            f"daily_halt={config.DAILY_LOSS_LIMIT_PCT:.0%} | "
            f"kill={config.TOTAL_DRAWDOWN_KILL_PCT:.0%}"
        )
        logger.info(
            f"Edge: threshold={config.MIN_EDGE_THRESHOLD:.0%} | "
            f"momentum_window={config.MOMENTUM_WINDOW_SECONDS}s | "
            f"order_cancel_timeout={config.ORDER_CANCEL_TIMEOUT}s"
        )
        logger.info("=" * 60)

        feed_task = asyncio.create_task(self.feed.run())

        logger.info(f"Warming up price feed ({WARMUP_SECONDS}s)…")
        await asyncio.sleep(WARMUP_SECONDS)

        btc = self.feed.get_price("BTCUSDT")
        if btc is None:
            logger.error("No BTC price data after warmup. Check network.")
            return

        logger.info(f"Feed active | BTC: ${btc:,.2f}")

        markets = self.poly.get_active_markets(force_refresh=True)
        logger.info(f"Loaded {len(markets)} active markets")

        bal = self.poly.get_balance()
        if bal is not None:
            logger.info(f"Polymarket USDC balance: ${bal:.2f}")

        tasks = [
            feed_task,
            asyncio.create_task(self._scan_loop()),
            asyncio.create_task(self._watchdog_loop()),
            asyncio.create_task(self._status_loop()),
            asyncio.create_task(self.settlement.run()),
        ]

        try:
            _ = await asyncio.gather(*tasks)
        except asyncio.CancelledError:
            logger.info("Shutdown signal received.")
        finally:
            await self._shutdown()

    # ── Scan loop ─────────────────────────────────────────────────────────────

    async def _scan_loop(self) -> None:
        while self._running:
            if not self.risk.is_live:
                logger.warning("Risk limits breached. Trading halted.")
                self._running = False
                break

            now = time.time()
            if now - self._last_scan >= SCAN_INTERVAL:
                self._last_scan = now
                try:
                    await self._scan_markets()
                except Exception as e:
                    logger.error(f"Scan error: {e}")

            await asyncio.sleep(0.05)

    async def _scan_markets(self) -> None:
        markets = self.poly.get_active_markets()
        for market in markets:
            if not self.risk.check_limits():
                break

            cid = market.get("condition_id", "")
            if not cid:
                continue

            if time.time() - self._cooldowns.get(cid, 0) < COOLDOWN_SECONDS:
                continue

            signal = self.edge.scan_market(market, self.risk)
            if signal:
                await self._execute(signal)

    # ── Order execution ───────────────────────────────────────────────────────

    async def _execute(self, signal: TradeSignal) -> None:
        if not self.risk.check_limits():
            return

        loop = asyncio.get_event_loop()
        order_id = await loop.run_in_executor(None, self.poly.place_order, signal)

        if order_id:
            placed_at = time.time()
            self.risk.record_open(signal.token_id, signal.size_usdc)
            self.settlement.track(signal.token_id, signal)
            self._cooldowns[signal.condition_id] = placed_at
            self._order_timestamps[signal.token_id] = TrackedOrder(
                order_id=order_id, placed_at=placed_at, signal=signal,
            )
        else:
            logger.warning(f"Order failed: {signal.market_id}")

    # ── Cancellation watchdog ─────────────────────────────────────────────────

    async def _watchdog_loop(self) -> None:
        """
        Runs every WATCHDOG_INTERVAL seconds.
        Cancels any order that hasn't filled within ORDER_CANCEL_TIMEOUT seconds.

        Why this matters:
          If our order doesn't fill in 8 seconds, the arbitrage window is closed.
          The position would now be a directional bet at a potentially bad price.
          Better to cancel and wait for the next signal.

        This should fire rarely — a properly priced GTC order near the ask
        should fill in under 2 seconds on a liquid market. Seeing this trigger
        frequently means your limit price is too conservative or the market
        is thinner than the liquidity filter allows.
        """
        while self._running:
            await asyncio.sleep(WATCHDOG_INTERVAL)
            now = time.time()

            stale = [
                (tid, tracked)
                for tid, tracked in list(self._order_timestamps.items())
                if now - tracked.placed_at > config.ORDER_CANCEL_TIMEOUT
            ]

            for token_id, tracked in stale:
                logger.warning(
                    f"[WATCHDOG] Cancelling stale order | "
                    f"{tracked.signal.symbol} {tracked.signal.direction.upper()} | "
                    f"id={tracked.order_id} | "
                    f"age={now - tracked.placed_at:.1f}s"
                )

                loop = asyncio.get_event_loop()
                await loop.run_in_executor(None, self.poly.cancel_order, tracked.order_id)

                # Clean up all tracking for this position
                _ = self._order_timestamps.pop(token_id, None)
                self.poly.remove_order_tracking(token_id)
                _ = self.settlement.pending.pop(token_id, None)

                # The capital was reserved in record_open — release it
                # by recording a $0 close (cancelled, no P&L)
                self.risk.record_close(token_id, 0.0, "cancelled", tracked.signal)

    # ── Status loop ───────────────────────────────────────────────────────────

    async def _status_loop(self) -> None:
        while self._running:
            await asyncio.sleep(300)
            logger.info(f"[STATUS] {self.risk.summary()}")

    # ── Shutdown ──────────────────────────────────────────────────────────────

    async def _shutdown(self) -> None:
        logger.info("Cancelling open orders…")
        self.poly.cancel_all()
        self.feed.stop()
        self.settlement.stop()
        logger.info(f"Final: {self.risk.summary()}")
        logger.info("Bot stopped.")

    def stop(self) -> None:
        self._running = False
        self.feed.stop()
        self.settlement.stop()
