"""M3-C planning persistence — PlanRun state machine regressions.

Pins the upgrade from inert request-scoped plans to a persisted PlanRun:
plan serialization round-trips, multi-step plans are stored with step status,
the latest run is queryable, and a still-running plan is resumable.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from echo_agent.agent.planning.models import Plan, PlanStep, StepStatus, StrategyType
from echo_agent.agent.planning.plan_run_store import PlanRunStore
from echo_agent.storage.sqlite import SQLiteBackend


def _plan() -> Plan:
    return Plan(
        strategy=StrategyType.PLAN_EXECUTE,
        goal="ship the feature",
        steps=[
            PlanStep(index=0, description="write code", tool_hint="edit_file"),
            PlanStep(index=1, description="run tests", tool_hint="exec"),
        ],
    )


# ── serialization ────────────────────────────────────────────────────────────


def test_plan_round_trip_preserves_step_status():
    plan = _plan()
    plan.mark_step_complete(0, "done")
    restored = Plan.from_dict(plan.to_dict())
    assert restored.goal == "ship the feature"
    assert restored.strategy == StrategyType.PLAN_EXECUTE
    assert restored.steps[0].status == StepStatus.COMPLETED
    assert restored.steps[0].result == "done"
    assert restored.steps[1].status == StepStatus.PENDING
    assert restored.current_step == 1


# ── PlanRunStore persistence ─────────────────────────────────────────────────


@pytest.mark.asyncio
async def test_plan_run_create_and_get_latest(tmp_path: Path):
    backend = SQLiteBackend(tmp_path / "s.db")
    await backend.initialize()
    try:
        store = PlanRunStore(backend)
        run_id = await store.create("sess1", "trace1", _plan())
        assert run_id
        run = await store.get_latest("sess1")
        assert run is not None
        assert run["status"] == "running"
        assert run["goal"] == "ship the feature"
        assert run["plan"] is not None
        assert len(run["plan"].steps) == 2
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_plan_run_update_marks_progress(tmp_path: Path):
    backend = SQLiteBackend(tmp_path / "s.db")
    await backend.initialize()
    try:
        store = PlanRunStore(backend)
        plan = _plan()
        run_id = await store.create("sess1", "trace1", plan)
        plan.mark_step_complete(0, "ok")
        await store.update(run_id, plan)
        run = await store.get_latest("sess1")
        assert run["current_step"] == 1
        assert run["plan"].steps[0].status == StepStatus.COMPLETED
        assert run["status"] == "running"  # one step left
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_plan_run_complete_status(tmp_path: Path):
    backend = SQLiteBackend(tmp_path / "s.db")
    await backend.initialize()
    try:
        store = PlanRunStore(backend)
        plan = _plan()
        run_id = await store.create("sess1", "trace1", plan)
        plan.mark_step_complete(0, "ok")
        plan.mark_step_complete(1, "ok")
        await store.update(run_id, plan)
        run = await store.get_latest("sess1")
        assert run["status"] == "complete"
        assert run["plan"].is_complete is True
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_resumable_returns_running_plan_only(tmp_path: Path):
    backend = SQLiteBackend(tmp_path / "s.db")
    await backend.initialize()
    try:
        store = PlanRunStore(backend)
        plan = _plan()
        run_id = await store.create("sess1", "trace1", plan)
        # Still running → resumable.
        resumable = await store.get_resumable("sess1")
        assert resumable is not None
        assert resumable.goal == "ship the feature"
        # Mark complete → no longer resumable.
        plan.mark_step_complete(0, "ok")
        plan.mark_step_complete(1, "ok")
        await store.update(run_id, plan)
        assert await store.get_resumable("sess1") is None
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_get_latest_none_for_unknown_session(tmp_path: Path):
    backend = SQLiteBackend(tmp_path / "s.db")
    await backend.initialize()
    try:
        store = PlanRunStore(backend)
        assert await store.get_latest("nope") is None
        assert await store.get_resumable("nope") is None
    finally:
        await backend.close()
