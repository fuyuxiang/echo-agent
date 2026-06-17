"""Dimensional cost ledger: migration, incremental accumulation, isolation."""

from __future__ import annotations

import inspect

import pytest

from echo_agent.cost.budget import CostTracker, BudgetExceeded
from echo_agent.storage.sqlite import SQLiteBackend


async def _fresh_storage(tmp_path):
    storage = SQLiteBackend(tmp_path / "db.sqlite")
    await storage.initialize()
    return storage


def _tracker(storage):
    return CostTracker(storage=storage, enabled=True, daily_budget_usd=100.0)


async def _dim_rows(storage):
    return await storage.fetch_sql(
        "SELECT window_date, provider, model, channel, spent_usd, "
        "input_tokens, output_tokens FROM cost_ledger_dim"
    )


@pytest.mark.asyncio
async def test_migration_creates_cost_ledger_dim(tmp_path):
    storage = SQLiteBackend(tmp_path / "db.sqlite")
    await storage.initialize()
    rows = await storage.fetch_sql(
        "SELECT name FROM sqlite_master WHERE type='table' AND name='cost_ledger_dim'"
    )
    assert len(rows) == 1
    await storage.close()


@pytest.mark.asyncio
async def test_record_writes_dim_row(tmp_path):
    storage = await _fresh_storage(tmp_path)
    t = _tracker(storage)
    await t.record("gpt-4o-mini", {"prompt_tokens": 1_000_000, "completion_tokens": 0},
                   "openai", channel="telegram")
    rows = await _dim_rows(storage)
    assert len(rows) == 1
    assert rows[0]["provider"] == "openai"
    assert rows[0]["model"] == "gpt-4o-mini"
    assert rows[0]["channel"] == "telegram"
    assert abs(rows[0]["spent_usd"] - 0.15) < 1e-9
    assert rows[0]["input_tokens"] == 1_000_000
    await storage.close()


@pytest.mark.asyncio
async def test_same_dimension_accumulates(tmp_path):
    storage = await _fresh_storage(tmp_path)
    t = _tracker(storage)
    usage = {"prompt_tokens": 1_000_000, "completion_tokens": 0}
    await t.record("gpt-4o-mini", usage, "openai", channel="telegram")
    await t.record("gpt-4o-mini", usage, "openai", channel="telegram")
    rows = await _dim_rows(storage)
    assert len(rows) == 1
    assert abs(rows[0]["spent_usd"] - 0.30) < 1e-9
    assert rows[0]["input_tokens"] == 2_000_000
    await storage.close()


@pytest.mark.asyncio
async def test_different_dimensions_isolated(tmp_path):
    storage = await _fresh_storage(tmp_path)
    t = _tracker(storage)
    usage = {"prompt_tokens": 1_000_000, "completion_tokens": 0}
    await t.record("gpt-4o-mini", usage, "openai", channel="telegram")
    await t.record("gpt-4o-mini", usage, "openai", channel="discord")
    rows = await _dim_rows(storage)
    assert len(rows) == 2
    await storage.close()


@pytest.mark.asyncio
async def test_channel_defaults_to_empty_string(tmp_path):
    storage = await _fresh_storage(tmp_path)
    t = _tracker(storage)
    await t.record("gpt-4o-mini", {"prompt_tokens": 1, "completion_tokens": 0}, "openai")
    rows = await _dim_rows(storage)
    assert len(rows) == 1
    assert rows[0]["channel"] == ""
    await storage.close()


class _DimFailStorage:
    """Storage that fails only on cost_ledger_dim writes; legacy writes succeed."""

    def __init__(self):
        self.legacy_writes = 0

    async def execute_sql(self, sql, params=()):
        if "cost_ledger_dim" in sql:
            raise RuntimeError("simulated dim write failure")
        self.legacy_writes += 1

    async def fetch_sql(self, sql, params=()):
        return []


@pytest.mark.asyncio
async def test_dim_failure_does_not_break_legacy_or_gate():
    storage = _DimFailStorage()
    t = CostTracker(storage=storage, enabled=True, daily_budget_usd=1.0)
    await t.record("gpt-4o-mini", {"prompt_tokens": 1_000_000, "completion_tokens": 0},
                   "openai", channel="telegram")
    assert abs(t.spent_usd - 0.15) < 1e-9
    assert storage.legacy_writes == 1
    t._spent_usd = 1.5
    with pytest.raises(BudgetExceeded):
        t.enforce()


@pytest.mark.asyncio
async def test_cross_day_writes_new_window_row(tmp_path):
    storage = await _fresh_storage(tmp_path)
    t = _tracker(storage)
    t._window_date = "2000-01-01"
    await t.record("gpt-4o-mini", {"prompt_tokens": 1_000_000, "completion_tokens": 0},
                   "openai", channel="telegram")
    rows = await _dim_rows(storage)
    assert all(r["window_date"] != "2000-01-01" for r in rows)
    await storage.close()


def test_record_accepts_channel_kwarg():
    sig = inspect.signature(CostTracker.record)
    assert "channel" in sig.parameters
    assert sig.parameters["channel"].default == ""
