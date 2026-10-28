"""Tests for echo_agent.evolution.evolver — Evolver."""

from __future__ import annotations

from pathlib import Path
from unittest.mock import AsyncMock, MagicMock

import pytest

from echo_agent.evolution.evolver import Evolver
from echo_agent.evolution.store import TrajectoryStore
from echo_agent.evolution.types import ToolCall, Trajectory
from echo_agent.storage.sqlite import SQLiteBackend


def _make_tc(arguments, name="propose_skill", tc_id="tc_1"):
    tc = MagicMock()
    tc.id = tc_id
    tc.name = name
    tc.arguments = arguments
    tc.to_openai_format.return_value = {
        "id": tc_id, "type": "function",
        "function": {"name": name, "arguments": arguments},
    }
    return tc


def _make_response(content="", tool_calls=None):
    r = MagicMock()
    r.content = content
    r.tool_calls = tool_calls or []
    r.has_tool_calls = bool(tool_calls)
    r.finish_reason = "stop"
    return r


def _make_skill_store(existing_names: list[str] | None = None):
    store = MagicMock()
    store.list_all.return_value = [
        type("M", (), {"name": n, "description": "x"})()
        for n in (existing_names or [])
    ]
    return store


def _make_traj(**overrides) -> Trajectory:
    base = dict(
        session_id="s",
        chat_id="c",
        channel="cli",
        task_input="t",
        task_type="chat",
        outcome="failure",
        iterations=1,
        tools_called=[ToolCall(name="x", args_digest="a", result_digest="r", success=False)],
        failure_reason="bad",
    )
    base.update(overrides)
    return Trajectory(**base)


async def _new_store(tmp_path: Path) -> tuple[TrajectoryStore, SQLiteBackend]:
    backend = SQLiteBackend(tmp_path / "evolution.db")
    await backend.initialize()
    s = TrajectoryStore(backend)
    await s.init_schema()
    return s, backend


@pytest.mark.asyncio
async def test_propose_create_skill_happy_path(tmp_path: Path):
    store, backend = await _new_store(tmp_path)
    try:
        skill_store = _make_skill_store(existing_names=["beta"])
        provider = AsyncMock()
        provider.chat_with_retry = AsyncMock(side_effect=[
            _make_response(tool_calls=[_make_tc({
                "operation": "create",
                "skill_name": "alpha",
                "content": "---\nname: alpha\ndescription: do X then verify\n---\n# Alpha\n",
                "rationale": "current trajectories fail without this skill",
                "expected_improvement": "chat tasks pass_rate from 0.5 to 0.7",
                "source_trajectory_ids": [],
            })]),
            _make_response(content="No more proposals."),
        ])

        evolver = Evolver(provider=provider, store=store, skill_store=skill_store, max_candidates=3)
        traj = _make_traj()
        decision = await evolver.propose([traj], run_id="run-1")
        assert len(decision.candidates) == 1
        cand = decision.candidates[0]
        assert cand.operation == "create"
        assert cand.skill_name == "alpha"
        assert cand.run_id == "run-1"
        assert traj.id in cand.source_trajectories
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_propose_rejects_create_with_existing_name(tmp_path: Path):
    store, backend = await _new_store(tmp_path)
    try:
        skill_store = _make_skill_store(existing_names=["alpha"])
        provider = AsyncMock()
        provider.chat_with_retry = AsyncMock(return_value=_make_response(content="done"))

        evolver = Evolver(provider=provider, store=store, skill_store=skill_store, max_candidates=3)
        result, err = evolver._build_candidate(
            args={
                "operation": "create",
                "skill_name": "alpha",
                "content": "---\nname: alpha\ndescription: x\n---\n",
                "rationale": "r",
                "expected_improvement": "i",
            },
            trajectories=[_make_traj()],
            run_id="r",
        )
        assert result is None
        assert "already exists" in err
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_propose_rejects_invalid_name(tmp_path: Path):
    store, backend = await _new_store(tmp_path)
    try:
        skill_store = _make_skill_store()
        provider = AsyncMock()
        evolver = Evolver(provider=provider, store=store, skill_store=skill_store)
        result, err = evolver._build_candidate(
            args={
                "operation": "create",
                "skill_name": "BAD NAME",
                "content": "---\nname: x\ndescription: y\n---",
                "rationale": "r",
                "expected_improvement": "i",
            },
            trajectories=[_make_traj()],
            run_id="r",
        )
        assert result is None
        assert "invalid skill_name" in err
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_propose_rejects_reserved_prefix(tmp_path: Path):
    store, backend = await _new_store(tmp_path)
    try:
        skill_store = _make_skill_store()
        provider = AsyncMock()
        evolver = Evolver(provider=provider, store=store, skill_store=skill_store)
        result, err = evolver._build_candidate(
            args={
                "operation": "create",
                "skill_name": "evolution-secret",
                "content": "---\nname: x\ndescription: y\n---",
                "rationale": "r",
                "expected_improvement": "i",
            },
            trajectories=[_make_traj()],
            run_id="r",
        )
        assert result is None
        assert "reserved prefix" in err
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_propose_rejects_oversize_content(tmp_path: Path):
    store, backend = await _new_store(tmp_path)
    try:
        skill_store = _make_skill_store()
        provider = AsyncMock()
        evolver = Evolver(
            provider=provider, store=store, skill_store=skill_store,
            skill_size_limit_bytes=200,
        )
        big_content = "---\nname: x\ndescription: y\n---\n" + ("a" * 500)
        result, err = evolver._build_candidate(
            args={
                "operation": "create",
                "skill_name": "smallish",
                "content": big_content,
                "rationale": "r",
                "expected_improvement": "i",
            },
            trajectories=[_make_traj()],
            run_id="r",
        )
        assert result is None
        assert "byte limit" in err
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_propose_patch_requires_existing_skill(tmp_path: Path):
    store, backend = await _new_store(tmp_path)
    try:
        skill_store = _make_skill_store(existing_names=["foo"])
        provider = AsyncMock()
        evolver = Evolver(provider=provider, store=store, skill_store=skill_store)

        ok, _ = evolver._build_candidate(
            args={
                "operation": "patch",
                "skill_name": "foo",
                "patch_old": "old",
                "patch_new": "new",
                "rationale": "r",
                "expected_improvement": "i",
            },
            trajectories=[_make_traj()],
            run_id="r",
        )
        assert ok is not None

        bad, err = evolver._build_candidate(
            args={
                "operation": "patch",
                "skill_name": "missing",
                "patch_old": "old",
                "patch_new": "new",
                "rationale": "r",
                "expected_improvement": "i",
            },
            trajectories=[_make_traj()],
            run_id="r",
        )
        assert bad is None
        assert "unknown skill" in err
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_propose_caps_at_max_candidates(tmp_path: Path):
    store, backend = await _new_store(tmp_path)
    try:
        skill_store = _make_skill_store()
        provider = AsyncMock()

        # The model keeps proposing forever; evolver must cap.
        def make_call(name):
            return _make_response(tool_calls=[_make_tc({
                "operation": "create",
                "skill_name": name,
                "content": "---\nname: x\ndescription: y\n---",
                "rationale": "r",
                "expected_improvement": "i",
            })])

        provider.chat_with_retry = AsyncMock(side_effect=[
            make_call("a"),
            make_call("b"),
            make_call("c"),
            make_call("d"),
        ])

        evolver = Evolver(provider=provider, store=store, skill_store=skill_store, max_candidates=2)
        decision = await evolver.propose([_make_traj()], run_id="r")
        assert len(decision.candidates) == 2
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_propose_no_trajectories_returns_empty(tmp_path: Path):
    store, backend = await _new_store(tmp_path)
    try:
        skill_store = _make_skill_store()
        provider = AsyncMock()
        evolver = Evolver(provider=provider, store=store, skill_store=skill_store)
        decision = await evolver.propose([], run_id="r")
        assert decision.candidates == []
        assert decision.consumed_trajectory_ids == []
        provider.chat_with_retry.assert_not_called()
    finally:
        await backend.close()
