"""Boundary tests for echo_agent.evolution.store.TrajectoryStore."""

from __future__ import annotations

from pathlib import Path
from unittest.mock import AsyncMock, MagicMock

import pytest

from echo_agent.evolution.store import TrajectoryStore
from echo_agent.evolution.types import (
    EvolutionRun,
    SkillCandidate,
    ToolCall,
    Trajectory,
)
from echo_agent.storage.sqlite import SQLiteBackend


def _make_traj(**overrides) -> Trajectory:
    base = dict(
        session_id="s",
        chat_id="c",
        channel="cli",
        task_input="t",
        task_type="chat",
        outcome="success",
        iterations=1,
        tools_called=[ToolCall(name="x")],
    )
    base.update(overrides)
    return Trajectory(**base)


async def _new_store(tmp_path: Path) -> tuple[TrajectoryStore, SQLiteBackend]:
    backend = SQLiteBackend(tmp_path / "evolution.db")
    await backend.initialize()
    s = TrajectoryStore(backend)
    await s.init_schema()
    return s, backend


# ── init_schema fault tolerance ────────────────────────────────────────────


@pytest.mark.asyncio
async def test_init_schema_propagates_storage_failure():
    """If the underlying storage fails during schema setup, the error must surface."""
    storage = MagicMock()
    storage.execute_sql = AsyncMock(side_effect=RuntimeError("disk full"))
    store = TrajectoryStore(storage)
    with pytest.raises(RuntimeError, match="disk full"):
        await store.init_schema()
    # Calling again should retry (initialised flag stays False).
    assert store._initialized is False


# ── Filtering combinations ───────────────────────────────────────────────────


@pytest.mark.asyncio
async def test_list_trajectories_combines_outcome_and_task_type(tmp_path: Path):
    store, backend = await _new_store(tmp_path)
    try:
        await store.append_trajectory(_make_traj(outcome="success", task_type="chat"))
        await store.append_trajectory(_make_traj(outcome="failure", task_type="chat"))
        await store.append_trajectory(_make_traj(outcome="failure", task_type="search"))
        result = await store.list_trajectories(outcome="failure", task_type="chat")
        assert len(result) == 1
        assert result[0].task_type == "chat"
        assert result[0].outcome == "failure"
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_list_trajectories_filter_since(tmp_path: Path):
    store, backend = await _new_store(tmp_path)
    try:
        old = _make_traj()
        old.created_at = "2000-01-01T00:00:00"
        new = _make_traj()
        new.created_at = "2030-01-01T00:00:00"
        await store.append_trajectory(old)
        await store.append_trajectory(new)
        # Cutoff between the two rows.
        recent = await store.list_trajectories(since="2020-01-01T00:00:00")
        assert {t.id for t in recent} == {new.id}
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_list_trajectories_limit_caps_results(tmp_path: Path):
    store, backend = await _new_store(tmp_path)
    try:
        for _ in range(10):
            await store.append_trajectory(_make_traj())
        capped = await store.list_trajectories(limit=3)
        assert len(capped) == 3
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_get_trajectory_returns_none_for_missing(tmp_path: Path):
    store, backend = await _new_store(tmp_path)
    try:
        assert await store.get_trajectory("never-existed") is None
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_get_candidate_returns_none_for_missing(tmp_path: Path):
    store, backend = await _new_store(tmp_path)
    try:
        assert await store.get_candidate("nope") is None
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_get_run_returns_none_for_missing(tmp_path: Path):
    store, backend = await _new_store(tmp_path)
    try:
        assert await store.get_run("nope") is None
        # latest_run on empty table returns None.
        assert await store.latest_run() is None
    finally:
        await backend.close()


# ── count_unconsumed ─────────────────────────────────────────────────────────


@pytest.mark.asyncio
async def test_count_unconsumed_returns_zero_when_table_empty(tmp_path: Path):
    store, backend = await _new_store(tmp_path)
    try:
        assert await store.count_unconsumed() == 0
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_count_unconsumed_handles_empty_rows():
    """Defensive: SQL returning empty list should yield 0."""
    storage = MagicMock()
    storage.fetch_sql = AsyncMock(return_value=[])
    storage.execute_sql = AsyncMock()
    store = TrajectoryStore(storage)
    assert await store.count_unconsumed() == 0


@pytest.mark.asyncio
async def test_count_unconsumed_handles_non_int_value():
    """Defensive: malformed COUNT result coerced to 0."""
    storage = MagicMock()
    storage.fetch_sql = AsyncMock(return_value=[{"n": "not-a-number"}])
    storage.execute_sql = AsyncMock()
    store = TrajectoryStore(storage)
    assert await store.count_unconsumed() == 0


# ── mark_consumed edge cases ────────────────────────────────────────────────


@pytest.mark.asyncio
async def test_mark_consumed_with_empty_list_is_noop(tmp_path: Path):
    store, backend = await _new_store(tmp_path)
    try:
        a = _make_traj()
        await store.append_trajectory(a)
        # Should not error and should not mark anything.
        await store.mark_consumed([], "run-1")
        assert await store.count_unconsumed() == 1
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_mark_consumed_preserved_through_re_append(tmp_path: Path):
    """Re-saving a trajectory must NOT clobber its consumed_run_id."""
    store, backend = await _new_store(tmp_path)
    try:
        t = _make_traj()
        await store.append_trajectory(t)
        await store.mark_consumed([t.id], "run-7")
        assert await store.count_unconsumed() == 0
        # Re-appending the same trajectory must keep the consumed marker.
        await store.append_trajectory(t)
        assert await store.count_unconsumed() == 0
    finally:
        await backend.close()


# ── purge_older_than edge cases ──────────────────────────────────────────────


@pytest.mark.asyncio
async def test_purge_with_zero_retention_is_noop(tmp_path: Path):
    store, backend = await _new_store(tmp_path)
    try:
        await store.append_trajectory(_make_traj())
        purged = await store.purge_older_than(0)
        assert purged == 0
        assert await store.count_unconsumed() == 1
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_purge_negative_retention_is_noop(tmp_path: Path):
    store, backend = await _new_store(tmp_path)
    try:
        await store.append_trajectory(_make_traj())
        assert await store.purge_older_than(-5) == 0
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_purge_handles_count_query_failure():
    """If the COUNT pre-query returns malformed data we still try to delete."""
    storage = MagicMock()
    # First fetch_sql is COUNT; return weird data so int() conversion goes through fallback.
    storage.fetch_sql = AsyncMock(return_value=[{"n": None}])
    storage.execute_sql = AsyncMock()
    store = TrajectoryStore(storage)
    # Should not raise; returns 0.
    purged = await store.purge_older_than(retention_days=30)
    assert purged == 0


# ── list_candidates filter combinations ──────────────────────────────────────


@pytest.mark.asyncio
async def test_list_candidates_filters_by_run_id(tmp_path: Path):
    store, backend = await _new_store(tmp_path)
    try:
        a = SkillCandidate(skill_name="alpha", run_id="run-A")
        b = SkillCandidate(skill_name="beta", run_id="run-B")
        await store.save_candidate(a)
        await store.save_candidate(b)
        only_a = await store.list_candidates(run_id="run-A")
        assert {c.id for c in only_a} == {a.id}
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_list_candidates_filters_by_skill_name(tmp_path: Path):
    store, backend = await _new_store(tmp_path)
    try:
        a = SkillCandidate(skill_name="alpha")
        b = SkillCandidate(skill_name="beta")
        await store.save_candidate(a)
        await store.save_candidate(b)
        only_alpha = await store.list_candidates(skill_name="alpha")
        assert {c.skill_name for c in only_alpha} == {"alpha"}
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_list_candidates_combined_filters(tmp_path: Path):
    store, backend = await _new_store(tmp_path)
    try:
        a = SkillCandidate(skill_name="alpha", run_id="r1", status="promoted")
        b = SkillCandidate(skill_name="alpha", run_id="r2", status="rejected")
        c = SkillCandidate(skill_name="beta", run_id="r1", status="promoted")
        for cand in (a, b, c):
            await store.save_candidate(cand)
        result = await store.list_candidates(
            skill_name="alpha", status="promoted",
        )
        assert {x.id for x in result} == {a.id}
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_latest_promoted_for_skill_returns_none_when_none_promoted(tmp_path: Path):
    store, backend = await _new_store(tmp_path)
    try:
        await store.save_candidate(SkillCandidate(skill_name="alpha", status="rejected"))
        assert await store.latest_promoted_for_skill("alpha") is None
    finally:
        await backend.close()


# ── update_candidate is alias of save_candidate ──────────────────────────────


@pytest.mark.asyncio
async def test_update_candidate_persists_status_change(tmp_path: Path):
    store, backend = await _new_store(tmp_path)
    try:
        cand = SkillCandidate(skill_name="alpha")
        await store.save_candidate(cand)
        cand.status = "rejected"
        cand.rejected_reason = "tested"
        await store.update_candidate(cand)
        refreshed = await store.get_candidate(cand.id)
        assert refreshed.status == "rejected"
        assert refreshed.rejected_reason == "tested"
    finally:
        await backend.close()


# ── list_runs respects limit ─────────────────────────────────────────────────


@pytest.mark.asyncio
async def test_list_runs_respects_limit(tmp_path: Path):
    store, backend = await _new_store(tmp_path)
    try:
        for _ in range(5):
            await store.save_run(EvolutionRun(triggered_by="manual"))
        runs = await store.list_runs(limit=2)
        assert len(runs) == 2
    finally:
        await backend.close()


# ── Malformed JSON tolerance ────────────────────────────────────────────────


@pytest.mark.asyncio
async def test_list_trajectories_skips_malformed_rows():
    """A bad JSON blob in one row must not poison the entire list."""
    storage = MagicMock()
    # First the count helper does not run for list_trajectories; only fetch_sql.
    good = _make_traj()
    import json as _json
    storage.fetch_sql = AsyncMock(return_value=[
        {"data": "not-json"},
        {"data": _json.dumps(good.to_dict())},
    ])
    storage.execute_sql = AsyncMock()
    store = TrajectoryStore(storage)
    rows = await store.list_trajectories()
    assert len(rows) == 1
    assert rows[0].id == good.id


@pytest.mark.asyncio
async def test_list_candidates_skips_malformed_rows():
    storage = MagicMock()
    import json as _json
    good = SkillCandidate(skill_name="alpha")
    storage.fetch_sql = AsyncMock(return_value=[
        {"data": "broken"},
        {"data": _json.dumps(good.to_dict())},
    ])
    storage.execute_sql = AsyncMock()
    store = TrajectoryStore(storage)
    rows = await store.list_candidates()
    assert len(rows) == 1
    assert rows[0].id == good.id


@pytest.mark.asyncio
async def test_list_runs_skips_malformed_rows():
    storage = MagicMock()
    import json as _json
    good = EvolutionRun(triggered_by="manual")
    storage.fetch_sql = AsyncMock(return_value=[
        {"data": "broken"},
        {"data": _json.dumps(good.to_dict())},
    ])
    storage.execute_sql = AsyncMock()
    store = TrajectoryStore(storage)
    rows = await store.list_runs()
    assert len(rows) == 1
    assert rows[0].id == good.id
