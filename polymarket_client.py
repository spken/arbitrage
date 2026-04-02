# pyright: reportUnknownMemberType=false, reportUnknownVariableType=false, reportUnknownArgumentType=false, reportAttributeAccessIssue=false, reportCallIssue=false
"""
polymarket_client.py

Wrapper around py-clob-client.

v2 changes:
  - get_resolved_position() — new method for settlement monitor
  - get_orderbook() now returns raw dict for EdgeDetector's depth analysis
  - Typed more explicitly to match what settlement_monitor expects
"""

import logging
import time
from typing import Any

from py_clob_client.client import ClobClient  # type: ignore[import-untyped]
from py_clob_client.clob_types import ApiCreds, OrderArgs, OrderType  # type: ignore[import-untyped]
from py_clob_client.constants import POLYGON  # type: ignore[import-untyped]

import config
from edge_detector import TradeSignal

logger = logging.getLogger(__name__)


class PolymarketClient:

    def __init__(self) -> None:
        creds = ApiCreds(
            api_key        = config.POLYMARKET_API_KEY,
            api_secret     = config.POLYMARKET_API_SECRET,
            api_passphrase = config.POLYMARKET_API_PASSPHRASE,
        )
        self._client: ClobClient = ClobClient(
            host     = config.POLYMARKET_HOST,
            chain_id = POLYGON,
            key      = config.PRIVATE_KEY,
            creds    = creds,
        )
        self._markets_cache: list[dict[str, Any]] = []
        self._cache_ts: float = 0.0
        self._active_orders: dict[str, str] = {}  # token_id → order_id

    # ── Market data ───────────────────────────────────────────────────────────

    def get_active_markets(self, force_refresh: bool = False) -> list[dict[str, Any]]:
        now = time.time()
        if force_refresh or (now - self._cache_ts) > config.MARKET_REFRESH_SECONDS:
            self._refresh_markets()
        return self._markets_cache

    def _refresh_markets(self) -> None:
        try:
            response: Any = self._client.get_markets()
            markets: list[Any] = (
                response.get("data", []) if isinstance(response, dict) else list(response)
            )

            filtered: list[dict[str, Any]] = []
            for m in markets:
                if not m.get("active") or m.get("closed"):
                    continue
                q: str = str(m.get("question", "")).lower()
                if not any(k in q for k in config.TARGET_KEYWORDS):
                    continue
                vol = float(m.get("volume", 0) or 0)
                if vol < config.MIN_MARKET_LIQUIDITY:
                    continue
                filtered.append(m)

            self._markets_cache = filtered
            self._cache_ts      = time.time()
            logger.debug(f"Market cache refreshed: {len(filtered)} active markets")

        except Exception as e:
            logger.error(f"Market refresh failed: {e}")

    def get_orderbook(self, token_id: str) -> dict[str, Any] | None:
        """Returns raw orderbook dict with 'bids' and 'asks' lists."""
        try:
            result: Any = self._client.get_order_book(token_id)
            return result  # type: ignore[no-any-return]
        except Exception as e:
            logger.warning(f"Orderbook fetch failed ({token_id[:12]}…): {e}")
            return None

    # ── Order management ─────────────────────────────────────────────────────

    def place_order(self, signal: TradeSignal) -> str | None:
        try:
            order: Any = self._client.create_order(
                OrderArgs(
                    token_id   = signal.token_id,
                    price      = signal.price,
                    size       = signal.size_usdc,
                    side       = "BUY",
                    order_type = OrderType.GTC,
                )
            )
            resp: Any = self._client.post_order(order)

            order_id: str | None = (
                resp.get("orderID")
                or resp.get("id")
                or resp.get("order", {}).get("id")
            )

            if order_id:
                self._active_orders[signal.token_id] = order_id
                logger.info(
                    f"[ORDER] {signal.symbol} {signal.direction.upper()} | "
                    f"price={signal.price:.4f} | size=${signal.size_usdc:.2f} | "
                    f"id={order_id}"
                )
                return order_id

            logger.warning(f"Order placed but no ID returned: {resp}")
            return None

        except Exception as e:
            logger.error(f"Order placement failed: {e}")
            return None

    def cancel_order(self, order_id: str) -> bool:
        try:
            self._client.cancel(order_id)
            logger.info(f"Order cancelled: {order_id}")
            return True
        except Exception as e:
            logger.warning(f"Cancel failed ({order_id}): {e}")
            return False

    def cancel_all(self) -> None:
        for _token_id, order_id in list(self._active_orders.items()):
            _ = self.cancel_order(order_id)
        self._active_orders.clear()

    def remove_order_tracking(self, token_id: str) -> None:
        """Remove a token from active order tracking (e.g. after watchdog cancel)."""
        _ = self._active_orders.pop(token_id, None)

    # ── Account & Positions ───────────────────────────────────────────────────

    def get_positions(self) -> list[dict[str, Any]]:
        """Returns all currently open positions."""
        try:
            result: Any = self._client.get_positions()
            return result or []  # type: ignore[no-any-return]
        except Exception as e:
            logger.warning(f"Position fetch failed: {e}")
            return []

    def get_resolved_position(self, token_id: str) -> dict[str, Any] | None:
        """
        Fetch resolved/redeemable position data for a specific token.
        Returns dict with 'payout' field, or None if not found / not yet resolved.
        """
        try:
            pos: Any = self._client.get_position(token_id)
            if pos and float(pos.get("redeemable", 0)) > 0:
                payout = float(pos["redeemable"])
                return {"payout": payout, "token_id": token_id}
            return None
        except Exception as e:
            logger.debug(f"Resolved position fetch: {e}")
            return None

    def get_balance(self) -> float | None:
        try:
            balance: Any = self._client.get_balance()
            return float(balance) if balance is not None else None
        except Exception as e:
            logger.warning(f"Balance fetch failed: {e}")
            return None
