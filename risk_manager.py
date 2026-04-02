"""
risk_manager.py

Enforces all position limits, drawdown rules, and Kelly sizing.
Every trade goes through this class before any order is placed.

Hard limits (non-negotiable):
  - Max 8% of portfolio per single trade
  - Halt trading at -20% daily loss
  - Kill bot permanently at -40% total drawdown
"""

import csv
import logging
import time
from datetime import date
from typing import TYPE_CHECKING

import config

if TYPE_CHECKING:
    from edge_detector import TradeSignal

logger = logging.getLogger(__name__)


class RiskManager:

    def __init__(self, starting_capital: float) -> None:
        self.starting_capital: float  = starting_capital
        self.portfolio_value: float   = starting_capital
        self.daily_start_value: float = starting_capital
        self._today: date             = date.today()
        self._killed: bool     = False
        self._halted_daily: bool = False
        self._open_positions: dict[str, float] = {}   # token_id -> USDC committed

        self._init_trade_log()

    # ── State ─────────────────────────────────────────────────────────────────

    @property
    def is_live(self) -> bool:
        """False if the bot should stop all trading activity."""
        return not self._killed and not self._halted_daily

    # ── Daily reset ───────────────────────────────────────────────────────────

    def _check_daily_reset(self) -> None:
        today = date.today()
        if today != self._today:
            logger.info(
                f"New day. Resetting daily PnL. Portfolio: ${self.portfolio_value:.2f}"
            )
            self.daily_start_value = self.portfolio_value
            self._today            = today
            self._halted_daily     = False

    # ── Limit checks ─────────────────────────────────────────────────────────

    def check_limits(self) -> bool:
        """
        Returns True if safe to place a trade.
        Call this before every order attempt.
        """
        self._check_daily_reset()

        if self._killed:
            logger.error("KILL SWITCH ACTIVE — bot is permanently stopped.")
            return False

        # ── Total drawdown ────────────────────────────────────────────────────
        total_dd = (self.starting_capital - self.portfolio_value) / self.starting_capital
        if total_dd >= config.TOTAL_DRAWDOWN_KILL_PCT:
            self._killed = True
            logger.critical(
                f"████ KILL SWITCH TRIGGERED ████ "
                f"Total drawdown: {total_dd:.1%} | "
                f"Portfolio: ${self.portfolio_value:.2f} | "
                f"Bot stopped permanently."
            )
            return False

        # ── Daily loss ────────────────────────────────────────────────────────
        if self.daily_start_value > 0:
            daily_dd = (self.daily_start_value - self.portfolio_value) / self.daily_start_value
            if daily_dd >= config.DAILY_LOSS_LIMIT_PCT:
                self._halted_daily = True
                logger.warning(
                    f"DAILY HALT | Daily loss: {daily_dd:.1%} | "
                    f"Resuming at next calendar day."
                )
                return False

        return True

    # ── Kelly sizing ──────────────────────────────────────────────────────────

    def kelly_size(self, win_prob: float, entry_price: float) -> float:
        """
        Fractional Kelly Criterion.

        entry_price: what we pay per share (0–1). Win payout = 1/entry_price - 1.
        Returns USDC position size, capped at MAX_POSITION_PCT of portfolio.
        """
        if not (0.01 < entry_price < 0.99):
            return 0.0

        b = (1.0 / entry_price) - 1.0    # net odds (profit per $1 risked)
        q = 1.0 - win_prob

        # Full Kelly fraction
        if b <= 0:
            return 0.0
        kelly_full = (b * win_prob - q) / b

        if kelly_full <= 0:
            return 0.0

        # Apply fractional Kelly + hard position cap
        fraction     = kelly_full * config.KELLY_FRACTION
        max_position = self.portfolio_value * config.MAX_POSITION_PCT
        size         = min(fraction * self.portfolio_value, max_position)

        return round(max(size, 0.0), 2)

    # ── Position tracking ─────────────────────────────────────────────────────

    def record_open(self, token_id: str, size_usdc: float) -> None:
        """Call immediately after a successful order placement."""
        self._open_positions[token_id] = (
            self._open_positions.get(token_id, 0.0) + size_usdc
        )
        logger.debug(f"Position opened: {token_id[:12]}… ${size_usdc:.2f}")

    def record_close(self, token_id: str, pnl: float, outcome: str, signal: "TradeSignal | None" = None) -> None:
        """
        Call when a position resolves (market settles on-chain).
        pnl: positive = profit, negative = loss (USDC)
        """
        _ = self._open_positions.pop(token_id, None)
        self.portfolio_value = round(self.portfolio_value + pnl, 4)

        emoji = "✅" if pnl > 0 else "❌"
        logger.info(
            f"{emoji} Closed | outcome={outcome} | pnl=${pnl:+.2f} | "
            f"portfolio=${self.portfolio_value:.2f}"
        )

        if signal:
            self._log_trade(signal, pnl, outcome)

        _ = self.check_limits()

    # ── Reporting ─────────────────────────────────────────────────────────────

    def summary(self) -> str:
        total_ret = (self.portfolio_value - self.starting_capital) / self.starting_capital
        daily_ret = (self.portfolio_value - self.daily_start_value) / self.daily_start_value
        return (
            f"Portfolio: ${self.portfolio_value:.2f} | "
            f"Total: {total_ret:+.2%} | "
            f"Daily: {daily_ret:+.2%} | "
            f"Open: {len(self._open_positions)} positions"
        )

    # ── CSV trade log ─────────────────────────────────────────────────────────

    def _init_trade_log(self) -> None:
        with open(config.TRADE_LOG_FILE, "w", newline="") as f:
            csv.writer(f).writerow([
                "timestamp", "market_id", "direction", "symbol",
                "price", "size_usdc", "edge", "p_model", "p_market",
                "momentum", "pnl", "portfolio_value", "outcome",
            ])

    def _log_trade(self, signal: "TradeSignal", pnl: float, outcome: str) -> None:
        with open(config.TRADE_LOG_FILE, "a", newline="") as f:
            csv.writer(f).writerow([
                time.strftime("%Y-%m-%d %H:%M:%S"),
                signal.market_id,
                signal.direction,
                signal.symbol,
                f"{signal.price:.4f}",
                f"{signal.size_usdc:.4f}",
                f"{signal.edge:.4f}",
                f"{signal.p_model:.4f}",
                f"{signal.p_market:.4f}",
                f"{signal.momentum:.6f}",
                f"{pnl:.4f}",
                f"{self.portfolio_value:.4f}",
                outcome,
            ])
