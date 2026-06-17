"""Stateful cost accumulation with a tiered daily budget gate."""

from __future__ import annotations

from datetime import date, datetime
from enum import Enum
from typing import Any

from loguru import logger

from echo_agent.cost.pricing import normalize_usage, estimate_cost


class BudgetStatus(str, Enum):
    OK = "ok"
    SOFT_EXCEEDED = "soft_exceeded"
    HARD_EXCEEDED = "hard_exceeded"


class BudgetExceeded(Exception):
    """Raised when the daily hard budget is exhausted."""


def _today_key() -> str:
    return date.today().isoformat()


class CostTracker:
    """Global daily-rolling cost accumulation + tiered enforcement gate.

    State is in-memory and persisted best-effort to cost_ledger. Designed for a
    single-process agent: accumulation is correct under asyncio concurrency
    (no await between read and += on _spent_usd), but multi-process deployments
    would need increment-based SQL or a shared lock since each process keeps its
    own in-memory total. If a persist fails it is swallowed, so a restart may
    reload a slightly under-counted total (budget under-estimated, never over).
    """

    def __init__(
        self, *, storage: Any = None, enabled: bool = False,
        daily_budget_usd: float = 0.0, soft_ratio: float = 0.8,
        pricing_overrides: dict | None = None,
    ):
        self._storage = storage
        self._enabled = enabled
        self._daily_budget_usd = max(0.0, float(daily_budget_usd))
        self._soft_ratio = min(1.0, max(0.0, float(soft_ratio)))
        self._overrides = pricing_overrides or {}
        self._window_date = _today_key()
        self._spent_usd = 0.0
        self._input = 0
        self._output = 0
        self._cache_read = 0
        self._cache_write = 0
        self._soft_warned = False

    @property
    def spent_usd(self) -> float:
        return self._spent_usd

    async def load(self) -> None:
        """Read today's accumulated total from storage (best-effort)."""
        if self._storage is None:
            return
        try:
            rows = await self._storage.fetch_sql(
                "SELECT spent_usd, input_tokens, output_tokens, cache_read_tokens, "
                "cache_write_tokens FROM cost_ledger WHERE window_date = ?",
                (self._window_date,),
            )
            if rows:
                r = rows[0]
                self._spent_usd = float(r["spent_usd"])
                self._input = int(r["input_tokens"])
                self._output = int(r["output_tokens"])
                self._cache_read = int(r["cache_read_tokens"])
                self._cache_write = int(r["cache_write_tokens"])
        except Exception:
            logger.warning("cost_ledger load failed; starting from in-memory zero")

    def _roll_window(self) -> None:
        today = _today_key()
        if today != self._window_date:
            self._window_date = today
            self._spent_usd = 0.0
            self._input = self._output = self._cache_read = self._cache_write = 0
            self._soft_warned = False

    async def record(self, model: str, usage: dict, provider: str = "", channel: str = "") -> None:
        """Normalize -> cost -> accumulate -> persist. Never raises."""
        try:
            self._roll_window()
            norm = normalize_usage(usage, provider)
            cost = estimate_cost(norm, model, self._overrides)
            self._spent_usd += cost
            self._input += norm.input
            self._output += norm.output
            self._cache_read += norm.cache_read
            self._cache_write += norm.cache_write
            await self._persist()
            await self._persist_dim(model, provider, channel, cost, norm)
            if self.check() == BudgetStatus.SOFT_EXCEEDED and not self._soft_warned:
                self._soft_warned = True
                logger.warning(
                    "Daily cost ${:.4f} crossed soft threshold ({:.0%} of ${:.2f}).",
                    self._spent_usd, self._soft_ratio, self._daily_budget_usd,
                )
        except Exception:
            logger.warning("cost record failed; metering skipped for this call")

    async def _persist(self) -> None:
        if self._storage is None:
            return
        try:
            await self._storage.execute_sql(
                "INSERT INTO cost_ledger (window_date, spent_usd, input_tokens, "
                "output_tokens, cache_read_tokens, cache_write_tokens, updated_at) "
                "VALUES (?, ?, ?, ?, ?, ?, ?) "
                "ON CONFLICT(window_date) DO UPDATE SET spent_usd=excluded.spent_usd, "
                "input_tokens=excluded.input_tokens, output_tokens=excluded.output_tokens, "
                "cache_read_tokens=excluded.cache_read_tokens, "
                "cache_write_tokens=excluded.cache_write_tokens, updated_at=excluded.updated_at",
                (self._window_date, self._spent_usd, self._input, self._output,
                 self._cache_read, self._cache_write, datetime.now().isoformat()),
            )
        except Exception:
            logger.warning("cost_ledger persist failed; kept in memory")

    async def _persist_dim(self, model: str, provider: str, channel: str, cost: float, norm) -> None:
        """Incremental per-dimension write for attribution/reporting.

        Independent best-effort: a failure here must never affect the legacy
        cost_ledger write or the budget gate. Reporting may miss data; the gate
        must not go blind.
        """
        if self._storage is None:
            return
        try:
            await self._storage.execute_sql(
                "INSERT INTO cost_ledger_dim (window_date, provider, model, channel, "
                "spent_usd, input_tokens, output_tokens, cache_read_tokens, "
                "cache_write_tokens, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?) "
                "ON CONFLICT(window_date, provider, model, channel) DO UPDATE SET "
                "spent_usd = spent_usd + excluded.spent_usd, "
                "input_tokens = input_tokens + excluded.input_tokens, "
                "output_tokens = output_tokens + excluded.output_tokens, "
                "cache_read_tokens = cache_read_tokens + excluded.cache_read_tokens, "
                "cache_write_tokens = cache_write_tokens + excluded.cache_write_tokens, "
                "updated_at = excluded.updated_at",
                (self._window_date, (provider or "").strip(), (model or "").strip(),
                 (channel or "").strip(), cost, norm.input, norm.output,
                 norm.cache_read, norm.cache_write, datetime.now().isoformat()),
            )
        except Exception:
            logger.warning("cost_ledger_dim persist failed; attribution skipped for this call")

    def check(self) -> BudgetStatus:
        self._roll_window()
        if not self._enabled or self._daily_budget_usd <= 0:
            return BudgetStatus.OK
        if self._spent_usd >= self._daily_budget_usd:
            return BudgetStatus.HARD_EXCEEDED
        if self._spent_usd >= self._daily_budget_usd * self._soft_ratio:
            return BudgetStatus.SOFT_EXCEEDED
        return BudgetStatus.OK

    def enforce(self) -> None:
        """Raise BudgetExceeded if the daily hard budget is exhausted."""
        self._roll_window()
        if self.check() == BudgetStatus.HARD_EXCEEDED:
            raise BudgetExceeded(
                f"今日成本预算 ${self._daily_budget_usd:.2f} 已用尽（已花 ${self._spent_usd:.4f}），将于明日重置。"
            )
