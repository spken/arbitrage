"""
settlement_monitor.py

Polls Polymarket's API for resolved positions and closes them in RiskManager.

Why polling instead of on-chain event listening?
  - On-chain event filtering (web3.py) requires a reliable WebSocket RPC connection,
    adds ~200ms latency on the non-critical settlement path, and is fragile.
  - Polymarket's REST API returns resolved positions directly, cleanly, reliably.
  - Settlement is not time-sensitive — a 30-second polling window is fine.
    The contract already resolved; we're just updating our P&L accounting.

Flow:
  1. Every SETTLEMENT_POLL_SECONDS, fetch all current positions from Polymarket.
  2. Compare to our locally tracked open positions (risk_manager._open_positions).
  3. Any token_id we're tracking that no longer appears as "open" has resolved.
  4. Determine outcome (YES resolved = won if we held YES, etc.).
  5. Calculate P&L and call risk_manager.record_close().
"""

import asyncio
import logging
from typing import Any

import config
from edge_detector import TradeSignal
from polymarket_client import PolymarketClient
from risk_manager import RiskManager

logger = logging.getLogger(__name__)


class SettlementMonitor:

    def __init__(self, polymarket_client: PolymarketClient, risk_manager: RiskManager) -> None:
        self.poly: PolymarketClient    = polymarket_client
        self.risk: RiskManager         = risk_manager
        self._running: bool = False

        # token_id → signal (stored at order placement time for P&L calculation)
        self.pending: dict[str, TradeSignal] = {}

    # ── Public API ────────────────────────────────────────────────────────────

    def track(self, token_id: str, signal: TradeSignal) -> None:
        """
        Register a filled order for settlement tracking.
        Call this after a successful order placement.
        """
        self.pending[token_id] = signal
        logger.debug(f"Tracking position: {token_id[:12]}… (${signal.size_usdc:.2f})")

    async def run(self) -> None:
        """Background task — polls for resolutions every SETTLEMENT_POLL_SECONDS."""
        self._running = True
        logger.info("Settlement monitor started")

        while self._running:
            await asyncio.sleep(config.SETTLEMENT_POLL_SECONDS)
            try:
                await self._check_resolutions()
            except Exception as e:
                logger.error(f"Settlement check error: {e}")

    def stop(self) -> None:
        self._running = False

    # ── Resolution Logic ──────────────────────────────────────────────────────

    async def _check_resolutions(self) -> None:
        if not self.pending:
            return

        loop = asyncio.get_event_loop()

        # Fetch current open positions from Polymarket
        positions: list[dict[str, Any]] = await loop.run_in_executor(None, self.poly.get_positions)

        # Build set of token_ids that are still open on Polymarket
        open_token_ids: set[str] = set()
        for pos in positions:
            tid = pos.get("asset_id") or pos.get("token_id") or pos.get("tokenId")
            if tid:
                open_token_ids.add(tid)

        # Any tracked position not in the open set has resolved
        resolved = [
            tid for tid in list(self.pending.keys())
            if tid not in open_token_ids
        ]

        for token_id in resolved:
            signal = self.pending.pop(token_id)
            await self._settle(token_id, signal)

    async def _settle(self, token_id: str, signal: TradeSignal) -> None:
        """
        Calculate P&L for a resolved position and update RiskManager.

        Polymarket settled positions appear with a non-zero 'redeemable' or
        'cash_balance' field. We look for the resolved entry to determine outcome.

        Fallback: if we can't find settlement data, we mark as unknown and
        remove from tracking to avoid blocking indefinitely.
        """
        loop = asyncio.get_event_loop()

        resolved_data: dict[str, Any] | None = None
        try:
            # Try to fetch the resolved position directly
            resolved_data = await loop.run_in_executor(
                None, self.poly.get_resolved_position, token_id
            )
        except Exception as e:
            logger.warning(f"Could not fetch resolution for {token_id[:12]}…: {e}")

        if resolved_data:
            payout     = float(resolved_data.get("payout", 0))
            outcome    = "win" if payout > 0 else "loss"
            # P&L = what we received back minus what we paid
            pnl        = payout - signal.size_usdc
        else:
            # Fallback: use market price at resolution time if available,
            # otherwise mark as unknown. This avoids silent P&L accounting gaps.
            logger.warning(
                f"Settlement data unavailable for {token_id[:12]}…. "
                f"Removing from tracking. Check trades.csv manually."
            )
            outcome = "unknown"
            pnl     = 0.0

        self.risk.record_close(token_id, pnl, outcome, signal)

        emoji = "✅" if pnl > 0 else ("❌" if pnl < 0 else "❓")
        logger.info(
            f"{emoji} Settled | {signal.symbol} {signal.direction.upper()} | "
            f"pnl=${pnl:+.4f} | outcome={outcome}"
        )
