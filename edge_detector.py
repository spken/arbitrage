"""
edge_detector.py

Detects mispricing between Polymarket contract odds and Binance price momentum.

v2 changes:
  - Momentum uses VWAP (volume-weighted average price) instead of raw price change.
    A 0.4% move on $5M volume is a real signal. A 0.4% move on $30k is noise.
  - Limit price uses orderbook depth (3rd ask level) instead of static +0.02 offset.
  - Orderbook depth check: requires 2× position size available before trading.

VWAP momentum:
  vwap_start = VWAP of first MIN_VOLUME_TICKS ticks in window
  vwap_end   = VWAP of last  MIN_VOLUME_TICKS ticks in window
  momentum   = (vwap_end - vwap_start) / vwap_start

This filters out low-volume noise while preserving real institutional momentum.
"""

import logging
import time
from dataclasses import dataclass
from typing import Any

import numpy as np

import config
from binance_feed import BinanceFeed
from polymarket_client import PolymarketClient
from risk_manager import RiskManager

logger = logging.getLogger(__name__)


@dataclass
class TradeSignal:
    market_id: str
    condition_id: str
    token_id: str
    direction: str
    price: float         # limit price derived from orderbook depth
    size_usdc: float
    edge: float
    p_model: float
    p_market: float
    symbol: str
    momentum: float
    total_volume: float  # total Binance volume in momentum window (diagnostic)


class EdgeDetector:

    def __init__(self, binance_feed: BinanceFeed, polymarket_client: PolymarketClient) -> None:
        self.feed: BinanceFeed = binance_feed
        self.poly: PolymarketClient = polymarket_client

    # ── VWAP Momentum ────────────────────────────────────────────────────────

    def _get_vwap_momentum(self, symbol: str) -> tuple[float, float] | None:
        """
        Returns (momentum_pct, total_volume) or None if insufficient data.

        Uses VWAP of first N and last N ticks in the window to filter noise.
        Low-volume ticks (noise, small retail trades) get proportionally less weight.
        """
        since = time.time() - config.MOMENTUM_WINDOW_SECONDS
        ticks = self.feed.get_ticks_since(f"{symbol}USDT", since)

        n = config.MIN_VOLUME_TICKS
        if len(ticks) < n * 2:
            return None

        # VWAP of first N ticks
        early = ticks[:n]
        vol_early = sum(t.volume for t in early)
        if vol_early == 0:
            return None
        vwap_start = sum(t.price * t.volume for t in early) / vol_early

        # VWAP of last N ticks
        recent = ticks[-n:]
        vol_recent = sum(t.volume for t in recent)
        if vol_recent == 0:
            return None
        vwap_end = sum(t.price * t.volume for t in recent) / vol_recent

        total_volume = sum(t.volume for t in ticks)
        momentum = (vwap_end - vwap_start) / vwap_start

        return momentum, total_volume

    def _momentum_to_probability(self, momentum: float, is_higher_market: bool) -> float:
        """Sigmoid maps VWAP momentum magnitude → win probability."""
        k   = config.SIGMOID_SENSITIVITY
        raw: float = 1.0 / (1.0 + float(np.exp(-k * abs(momentum))))

        if is_higher_market:
            return raw if momentum > 0 else 1.0 - raw
        else:
            return raw if momentum < 0 else 1.0 - raw

    # ── Limit Price from Orderbook ────────────────────────────────────────────

    def _get_limit_price(
        self,
        token_id: str,
        _side: str,
        required_size: float,
    ) -> float | None:
        """
        Fetches the live orderbook and sets limit price at the Nth ask level.

        Why Nth level instead of +0.02 static offset?
        - In liquid markets, the 3rd ask might only be +0.005 away → we pay less.
        - In thin markets, it might be +0.06 away → we know upfront the real cost.
        - Static offsets either overpay in liquid markets or underpay and miss fills.

        Also checks that there's at least ORDERBOOK_MIN_DEPTH × size available
        before the Nth level. If not, the market is too thin to exit cleanly.

        Returns None if the book is too thin or unavailable.
        """
        book = self.poly.get_orderbook(token_id)
        if not book:
            return None

        asks: list[dict[str, Any]] = sorted(book.get("asks", []), key=lambda x: float(x["price"]))
        if len(asks) < config.ORDERBOOK_DEPTH_LEVEL:
            return None

        # Verify sufficient depth up to our target level
        cumulative_size = 0.0
        for ask in asks[:config.ORDERBOOK_DEPTH_LEVEL]:
            try:
                cumulative_size += float(ask.get("size", 0))
            except (ValueError, TypeError):
                pass

        min_required = required_size * config.ORDERBOOK_MIN_DEPTH
        if cumulative_size < min_required:
            logger.debug(
                f"Book too thin: {cumulative_size:.1f} USDC available, "
                f"need {min_required:.1f}"
            )
            return None

        # Use Nth ask level price, capped at 0.96 to preserve minimum payout
        target_price = float(asks[config.ORDERBOOK_DEPTH_LEVEL - 1]["price"])
        return min(round(target_price, 4), 0.96)

    # ── Market Scanning ───────────────────────────────────────────────────────

    def scan_market(self, market: dict[str, Any], risk_manager: RiskManager) -> TradeSignal | None:
        """
        Full scan pipeline for one market.
        Returns TradeSignal if exploitable edge found, else None.
        """
        question: str = market.get("question", "").lower()

        # Identify asset
        symbol: str | None = None
        if "btc" in question or "bitcoin" in question:
            symbol = "BTC"
        elif "eth" in question or "ethereum" in question:
            symbol = "ETH"
        elif "sol" in question or "solana" in question:
            symbol = "SOL"
        else:
            return None

        is_higher_market = any(w in question for w in ["higher", "above", "over", "up"])

        if not self.feed.has_data(f"{symbol}USDT", min_ticks=config.MIN_VOLUME_TICKS * 2):
            return None

        result = self._get_vwap_momentum(symbol)
        if result is None:
            return None

        momentum, total_volume = result

        if abs(momentum) < config.MOMENTUM_THRESHOLD:
            return None

        tokens: list[dict[str, Any]] = market.get("tokens", [])
        if len(tokens) < 2:
            return None

        yes_token = next(
            (t for t in tokens if t.get("outcome", "").lower() == "yes"), tokens[0]
        )
        no_token = next(
            (t for t in tokens if t.get("outcome", "").lower() == "no"), tokens[1]
        )

        try:
            p_market_yes = float(yes_token.get("price", 0.5))
            p_market_no  = float(no_token.get("price", 0.5))
        except (TypeError, ValueError):
            return None

        if not (0.02 <= p_market_yes <= 0.98):
            return None

        p_model_yes = self._momentum_to_probability(momentum, is_higher_market)

        edge_yes = p_model_yes - p_market_yes
        edge_no  = (1.0 - p_model_yes) - p_market_no

        if max(edge_yes, edge_no) < config.MIN_EDGE_THRESHOLD:
            return None

        # ── Choose direction ─────────────────────────────────────────────────
        if edge_yes >= edge_no:
            direction  = "yes"
            token_id: str = yes_token["token_id"]
            p_market   = p_market_yes
            p_model    = p_model_yes
            best_edge  = edge_yes
        else:
            direction  = "no"
            token_id = no_token["token_id"]
            p_market   = p_market_no
            p_model    = 1.0 - p_model_yes
            best_edge  = edge_no

        # ── Preliminary Kelly size (needed for depth check) ──────────────────
        # Use a rough estimate first; will refine once we have the real price
        rough_price = p_market + 0.02
        prelim_size = risk_manager.kelly_size(p_model, rough_price)
        if prelim_size < 1.0:
            return None

        # ── Orderbook-based limit price ──────────────────────────────────────
        limit_price = self._get_limit_price(token_id, direction, prelim_size)
        if limit_price is None:
            return None

        # ── Final Kelly size with accurate limit price ───────────────────────
        size = risk_manager.kelly_size(p_model, limit_price)
        if size < 1.0:
            return None

        logger.info(
            f"[EDGE] {symbol} {direction.upper()} | "
            f"vwap_momentum={momentum:+.4f} | vol={total_volume:.2f} | "
            f"p_model={p_model:.3f} | p_market={p_market:.3f} | "
            f"edge={best_edge:.3f} | limit={limit_price:.4f} | size=${size:.2f}"
        )

        return TradeSignal(
            market_id    = market.get("market_slug", market.get("id", "")),
            condition_id = market.get("condition_id", ""),
            token_id     = token_id,
            direction    = direction,
            price        = limit_price,
            size_usdc    = size,
            edge         = best_edge,
            p_model      = p_model,
            p_market     = p_market,
            symbol       = symbol,
            momentum     = momentum,
            total_volume = total_volume,
        )
