"""Tests for echo_agent.evolution.store — TrajectoryStore against real SQLite."""

from __future__ import annotations

from pathlib import Path

import pytest

from echo_agent.evolution.store import TrajectoryStore
from echo_agent.evolution.types import (
    EvolutionRun,
    SkillCandidate,
    ToolCall,
    Trajectory,
)
from echo_agent.storage.sqlite import SQLiteBackend


async def _new_store(tmp_path: Path) -> tuple[TrajectoryStore, SQLiteBackend]:
    backend = SQLiteBackend(tmp_path / "evolution.db")
    await backend.initialize()
    s = TrajectoryStore(backend)
    await s.init_schema()
    return s, backend


def _make_traj(**overrides) -> Trajectory:
    base = dict(
        session_id="s",
        chat_id="c",
        channel="cli",
        task_input="hi",
        task_type="chat",
        outcome="success",
        iterations=1,
        tools_called=[ToolCall(name="x", args_digest="a", result_digest="r")],
    )
    base.update(overrides)
    return Trajectory(**base)


@pytest.mark.asyncio
async def test_init_schema_is_idempotent(tmp_path: Path):
    store, backend = await _new_store(tmp_path)
    try:
        await store.init_schema()
        await store.init_schema()
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_append_and_get_trajectory(tmp_path: Path):
    store, backend = await _new_store(tmp_path)
    try:
        t = _make_traj()
        await store.append_trajectory(t)
        found = await store.get_trajectory(t.id)
        assert found is not None
        assert found.id == t.id
        assert found.outcome == "success"
        assert found.tools_called[0].name == "x"
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_count_unconsumed_excludes_consumed_rows(tmp_path: Path):
    store, backend = await _new_store(tmp_path)
    try:
        a = _make_traj()
        b = _make_traj()
        await store.append_trajectory(a)
        await store.append_trajectory(b)
        assert await store.count_unconsumed() == 2
        await store.mark_consumed([a.id], "run-1")
        assert await store.count_unconsumed() == 1
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_list_trajectories_filters_by_outcome_and_unconsumed(tmp_path: Path):
    store, backend = await _new_store(tmp_path)
    try:
        success = _make_traj(outcome="success")
        failure = _make_traj(outcome="failure", task_input="boom")
        await store.append_trajectory(success)
        await store.append_trajectory(failure)
        failures = await store.list_trajectories(outcome="failure")
        assert len(failures) == 1
        assert failures[0].outcome == "failure"

        await store.mark_consumed([success.id], "run-1")
        unconsumed = await store.list_trajectories(only_unconsumed=True)
        assert {t.id for t in unconsumed} == {failure.id}
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_save_and_list_candidates(tmp_path: Path):
    store, backend = await _new_store(tmp_path)
    try:
        c = SkillCandidate(
            operation="create",
            skill_name="alpha",
            proposed_content="---\nname: alpha\ndescription: x\n---\n",
        )
        await store.save_candidate(c)
        pending = await store.list_candidates(status="pending")
        assert len(pending) == 1
        assert pending[0].skill_name == "alpha"

        c.status = "promoted"
        await store.update_candidate(c)
        promoted = await store.list_candidates(status="promoted")
        assert len(promoted) == 1
        assert promoted[0].id == c.id
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_latest_promoted_for_skill(tmp_path: Path):
    store, backend = await _new_store(tmp_path)
    try:
        a = SkillCandidate(operation="create", skill_name="alpha")
        b = SkillCandidate(operation="patch", skill_name="alpha")
        a.status = "promoted"
        b.status = "promoted"
        await store.save_candidate(a)
        await store.save_candidate(b)
        latest = await store.latest_promoted_for_skill("alpha")
        assert latest is not None
        assert latest.id == b.id
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_save_and_load_run(tmp_path: Path):
    store, backend = await _new_store(tmp_path)
    try:
        r = EvolutionRun(triggered_by="manual", trajectories_consumed=3)
        await store.save_run(r)
        loaded = await store.get_run(r.id)
        assert loaded is not None
        assert loaded.triggered_by == "manual"
        assert loaded.trajectories_consumed == 3
        latest = await store.latest_run()
        assert latest is not None
        assert latest.id == r.id
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_purge_older_than(tmp_path: Path):
    store, backend = await _new_store(tmp_path)
    try:
        t = _make_traj()
        t.created_at = "1990-01-01T00:00:00"
        await store.append_trajectory(t)
        purged = await store.purge_older_than(retention_days=1)
        assert purged == 1
        assert await store.count_unconsumed() == 0
    finally:
        await backend.close()
