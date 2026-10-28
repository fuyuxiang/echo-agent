"""CostTracker: accumulation, daily reset, tiered gate."""

from __future__ import annotations

import pytest

from echo_agent.config.schema import Config, CostConfig
from echo_agent.cost.budget import CostTracker, BudgetStatus, BudgetExceeded
from echo_agent.storage.sqlite import SQLiteBackend


def _tracker(**kw):
    # storage=None -> in-memory only (persistence covered separately).
    defaults = dict(storage=None, enabled=True, daily_budget_usd=1.0, soft_ratio=0.8)
    defaults.update(kw)
    return CostTracker(**defaults)


@pytest.mark.asyncio
async def test_record_accumulates_cost():
    t = _tracker()
    await t.record("gpt-4o-mini", {"prompt_tokens": 1_000_000, "completion_tokens": 0}, "openai")
    assert abs(t.spent_usd - 0.15) < 1e-9


@pytest.mark.asyncio
async def test_check_ok_soft_hard():
    t = _tracker(daily_budget_usd=1.0, soft_ratio=0.8)
    assert t.check() == BudgetStatus.OK
    t._spent_usd = 0.85
    assert t.check() == BudgetStatus.SOFT_EXCEEDED
    t._spent_usd = 1.5
    assert t.check() == BudgetStatus.HARD_EXCEEDED


@pytest.mark.asyncio
async def test_check_disabled_never_hard():
    t = _tracker(enabled=False)
    t._spent_usd = 999.0
    assert t.check() == BudgetStatus.OK


@pytest.mark.asyncio
async def test_check_zero_budget_never_hard():
    t = _tracker(daily_budget_usd=0.0)
    t._spent_usd = 999.0
    assert t.check() == BudgetStatus.OK


@pytest.mark.asyncio
async def test_daily_window_reset():
    t = _tracker()
    t._spent_usd = 0.9
    t._window_date = "2000-01-01"
    await t.record("gpt-4o-mini", {"prompt_tokens": 0, "completion_tokens": 0}, "openai")
    assert t._window_date != "2000-01-01"
    assert t.spent_usd == 0.0


@pytest.mark.asyncio
async def test_enforce_raises_on_hard():
    t = _tracker()
    t._spent_usd = 2.0
    with pytest.raises(BudgetExceeded):
        t.enforce()


@pytest.mark.asyncio
async def test_check_rolls_window_on_new_day():
    # A long-lived process that exceeded budget yesterday must not keep
    # reporting HARD today when check() is called directly (no record/enforce).
    t = _tracker(daily_budget_usd=1.0)
    t._spent_usd = 5.0
    t._window_date = "2000-01-01"  # stale window from a previous day
    assert t.check() == BudgetStatus.OK  # new day -> rolled to zero -> OK
    assert t.spent_usd == 0.0


@pytest.mark.asyncio
async def test_persistence_round_trip(tmp_path):
    backend = SQLiteBackend(tmp_path / "cost.db")
    await backend.initialize()
    try:
        t1 = CostTracker(storage=backend, enabled=True, daily_budget_usd=10.0)
        await t1.load()
        await t1.record("gpt-4o-mini", {"prompt_tokens": 2_000_000, "completion_tokens": 0}, "openai")
        spent = t1.spent_usd
        assert spent > 0

        # New tracker, same backend & day -> load() should read accumulated total.
        t2 = CostTracker(storage=backend, enabled=True, daily_budget_usd=10.0)
        await t2.load()
        assert abs(t2.spent_usd - spent) < 1e-9
    finally:
        await backend.close()


def test_cost_config_defaults():
    c = Config()
    assert c.cost.enabled is False
    assert c.cost.daily_budget_usd == 0.0
    assert c.cost.soft_threshold_ratio == 0.8
    assert c.cost.pricing_overrides == {}


def test_cost_config_camel_alias():
    c = CostConfig(dailyBudgetUsd=5.0, softThresholdRatio=0.9)
    assert c.daily_budget_usd == 5.0
    assert c.soft_threshold_ratio == 0.9
