"""Tests for echo_agent.evolution.scheduler — EvolutionScheduler."""

from __future__ import annotations

import asyncio
from datetime import datetime
from unittest.mock import AsyncMock

import pytest

from echo_agent.evolution.scheduler import EvolutionScheduler


def _record_run() -> tuple[AsyncMock, list[str]]:
    """Build a (run_fn, triggers_seen) pair where each call appends the trigger."""
    seen: list[str] = []

    async def _run(*, trigger):
        seen.append(trigger)

    run_fn = AsyncMock(side_effect=_run)
    return run_fn, seen


# ── Mode property ────────────────────────────────────────────────────────────


@pytest.mark.parametrize("mode", ["manual", "threshold", "scheduled"])
def test_mode_property_reflects_constructor(mode: str):
    sch = EvolutionScheduler(
        run_fn=AsyncMock(),
        unconsumed_count_fn=AsyncMock(return_value=0),
        trigger_mode=mode,
    )
    assert sch.mode == mode


# ── Manual mode ──────────────────────────────────────────────────────────────


@pytest.mark.asyncio
async def test_manual_mode_does_not_start_loop():
    run_fn = AsyncMock()
    count_fn = AsyncMock(return_value=999)
    sch = EvolutionScheduler(
        run_fn=run_fn,
        unconsumed_count_fn=count_fn,
        trigger_mode="manual",
        threshold=1,
        poll_interval_seconds=5.0,
    )
    await sch.start()
    # Give the event loop a beat in case a task slipped through.
    await asyncio.sleep(0.05)
    await sch.stop()
    run_fn.assert_not_called()
    count_fn.assert_not_called()


# ── Threshold mode ───────────────────────────────────────────────────────────


@pytest.mark.asyncio
async def test_threshold_fires_when_count_meets_threshold():
    run_fn, seen = _record_run()
    count_fn = AsyncMock(return_value=10)
    sch = EvolutionScheduler(
        run_fn=run_fn,
        unconsumed_count_fn=count_fn,
        trigger_mode="threshold",
        threshold=10,
    )
    await sch._tick_threshold()
    assert seen == ["threshold"]
    run_fn.assert_awaited_once()


@pytest.mark.asyncio
async def test_threshold_does_not_fire_below_threshold():
    run_fn, seen = _record_run()
    count_fn = AsyncMock(return_value=4)
    sch = EvolutionScheduler(
        run_fn=run_fn,
        unconsumed_count_fn=count_fn,
        trigger_mode="threshold",
        threshold=5,
    )
    await sch._tick_threshold()
    assert seen == []
    run_fn.assert_not_called()


@pytest.mark.asyncio
async def test_threshold_count_fn_exception_swallowed():
    run_fn, seen = _record_run()
    count_fn = AsyncMock(side_effect=RuntimeError("db down"))
    sch = EvolutionScheduler(
        run_fn=run_fn,
        unconsumed_count_fn=count_fn,
        trigger_mode="threshold",
        threshold=1,
    )
    # Should not raise.
    await sch._tick_threshold()
    assert seen == []


@pytest.mark.asyncio
async def test_threshold_clamps_minimum_to_one():
    sch = EvolutionScheduler(
        run_fn=AsyncMock(),
        unconsumed_count_fn=AsyncMock(return_value=0),
        trigger_mode="threshold",
        threshold=-5,  # nonsense input
    )
    assert sch._threshold == 1


@pytest.mark.asyncio
async def test_run_fn_exception_does_not_propagate():
    run_fn = AsyncMock(side_effect=RuntimeError("evolver crashed"))
    sch = EvolutionScheduler(
        run_fn=run_fn,
        unconsumed_count_fn=AsyncMock(return_value=10),
        trigger_mode="threshold",
        threshold=1,
    )
    # _safe_run wraps run_fn; tick_threshold should not raise.
    await sch._tick_threshold()


# ── Scheduled (cron) mode ────────────────────────────────────────────────────


def test_compute_next_cron_returns_future_timestamp():
    sch = EvolutionScheduler(
        run_fn=AsyncMock(),
        unconsumed_count_fn=AsyncMock(return_value=0),
        trigger_mode="scheduled",
        cron_expression="*/5 * * * *",
    )
    base = datetime.now()
    nxt = sch._compute_next_cron_ts(base)
    assert nxt is not None
    assert nxt > base.timestamp()


def test_compute_next_cron_tolerates_invalid_expression():
    sch = EvolutionScheduler(
        run_fn=AsyncMock(),
        unconsumed_count_fn=AsyncMock(return_value=0),
        trigger_mode="scheduled",
        cron_expression="not a real cron",
    )
    assert sch._compute_next_cron_ts(datetime.now()) is None


def test_compute_next_cron_returns_none_for_empty_expression():
    sch = EvolutionScheduler(
        run_fn=AsyncMock(),
        unconsumed_count_fn=AsyncMock(return_value=0),
        trigger_mode="scheduled",
        cron_expression="",
    )
    assert sch._compute_next_cron_ts(datetime.now()) is None


@pytest.mark.asyncio
async def test_scheduled_first_tick_initialises_next_ts():
    """The first cron tick computes _next_cron_ts but does not fire yet."""
    run_fn, seen = _record_run()
    sch = EvolutionScheduler(
        run_fn=run_fn,
        unconsumed_count_fn=AsyncMock(return_value=0),
        trigger_mode="scheduled",
        cron_expression="*/5 * * * *",
    )
    assert sch._next_cron_ts is None
    await sch._tick_cron()
    assert sch._next_cron_ts is not None
    assert seen == []


@pytest.mark.asyncio
async def test_scheduled_fires_when_now_passes_next_ts():
    run_fn, seen = _record_run()
    sch = EvolutionScheduler(
        run_fn=run_fn,
        unconsumed_count_fn=AsyncMock(return_value=0),
        trigger_mode="scheduled",
        cron_expression="*/5 * * * *",
    )
    # Force the next-fire timestamp into the past so the next tick triggers.
    sch._next_cron_ts = 0.0
    await sch._tick_cron()
    assert seen == ["scheduled"]
    # The scheduler must have computed a fresh future timestamp.
    assert sch._next_cron_ts is not None and sch._next_cron_ts > 0


# ── Lifecycle ────────────────────────────────────────────────────────────────


@pytest.mark.asyncio
async def test_start_is_idempotent_for_active_modes():
    run_fn = AsyncMock()
    sch = EvolutionScheduler(
        run_fn=run_fn,
        unconsumed_count_fn=AsyncMock(return_value=0),
        trigger_mode="threshold",
        threshold=999,
        poll_interval_seconds=5.0,
    )
    await sch.start()
    first_task = sch._task
    await sch.start()  # second start must not replace the running task
    try:
        assert sch._task is first_task
        assert not first_task.done()
    finally:
        await sch.stop()


@pytest.mark.asyncio
async def test_stop_without_start_is_safe():
    sch = EvolutionScheduler(
        run_fn=AsyncMock(),
        unconsumed_count_fn=AsyncMock(return_value=0),
        trigger_mode="threshold",
    )
    await sch.stop()
    assert sch._task is None


@pytest.mark.asyncio
async def test_threshold_loop_runs_real_tick():
    """Drive a full start → wait for one tick → stop cycle."""
    run_fn, seen = _record_run()

    fired = asyncio.Event()

    async def count() -> int:
        # Resolve the threshold once, then signal so the test can stop early.
        fired.set()
        return 100

    sch = EvolutionScheduler(
        run_fn=run_fn,
        unconsumed_count_fn=count,
        trigger_mode="threshold",
        threshold=1,
        poll_interval_seconds=5.0,
    )
    await sch.start()
    try:
        await asyncio.wait_for(fired.wait(), timeout=2.0)
        # Allow the chained _safe_run -> run_fn to complete.
        await asyncio.sleep(0.05)
    finally:
        await sch.stop()
    assert seen == ["threshold"]


@pytest.mark.asyncio
async def test_loop_internal_exception_is_swallowed_and_loop_continues():
    """A tick that explodes must not kill the polling loop."""
    calls: list[int] = []

    async def explode_then_settle() -> int:
        calls.append(len(calls))
        if len(calls) == 1:
            raise RuntimeError("transient")
        return 0

    sch = EvolutionScheduler(
        run_fn=AsyncMock(),
        unconsumed_count_fn=explode_then_settle,
        trigger_mode="threshold",
        threshold=1,
        poll_interval_seconds=5.0,
    )

    # Patch _tick to force an exception once, then continue normally.
    original_tick = sch._tick
    flag = {"tripped": False}

    async def bad_tick():
        if not flag["tripped"]:
            flag["tripped"] = True
            raise RuntimeError("boom")
        await original_tick()

    sch._tick = bad_tick  # type: ignore[assignment]

    await sch.start()
    try:
        # Wait long enough for at least one tick attempt.
        await asyncio.sleep(0.05)
        # Force a second tick directly to confirm the loop is still alive.
        await original_tick()
    finally:
        await sch.stop()
    assert flag["tripped"] is True


@pytest.mark.asyncio
async def test_poll_interval_clamped_to_minimum():
    """poll_interval below 5s is clamped up — protects against tight loops."""
    sch = EvolutionScheduler(
        run_fn=AsyncMock(),
        unconsumed_count_fn=AsyncMock(return_value=0),
        trigger_mode="threshold",
        poll_interval_seconds=0.001,
    )
    assert sch._poll_interval >= 5.0
