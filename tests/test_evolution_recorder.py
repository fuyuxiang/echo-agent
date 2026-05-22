"""Tests for echo_agent.evolution.recorder — TrajectoryRecorder."""

from __future__ import annotations

from pathlib import Path
from typing import Any
from unittest.mock import AsyncMock, MagicMock

import pytest

from echo_agent.evolution.recorder import TrajectoryRecorder
from echo_agent.evolution.store import TrajectoryStore
from echo_agent.plugins.hooks import HookRegistry
from echo_agent.storage.sqlite import SQLiteBackend


async def _make_recorder(tmp_path: Path) -> tuple[TrajectoryRecorder, TrajectoryStore, SQLiteBackend]:
    backend = SQLiteBackend(tmp_path / "evolution.db")
    await backend.initialize()
    store = TrajectoryStore(backend)
    await store.init_schema()
    skill_store = MagicMock()
    skill_store.list_all.return_value = [
        type("M", (), {"name": "alpha"})(),
        type("M", (), {"name": "beta"})(),
    ]
    recorder = TrajectoryRecorder(store, skill_store=skill_store)
    return recorder, store, backend


def _ctx(session_key: str) -> Any:
    ctx = MagicMock()
    ctx.session_key = session_key
    return ctx


@pytest.mark.asyncio
async def test_attach_registers_three_hooks(tmp_path: Path):
    recorder, _, backend = await _make_recorder(tmp_path)
    try:
        hooks = HookRegistry()
        recorder.attach(hooks)
        registered = hooks.get_registered_hooks()
        assert "post_tool_call" in registered
        assert "post_llm_call" in registered
        assert "on_error" in registered
        for name in ("post_tool_call", "post_llm_call", "on_error"):
            assert "evolution" in registered[name]
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_attach_is_idempotent(tmp_path: Path):
    recorder, _, backend = await _make_recorder(tmp_path)
    try:
        hooks = HookRegistry()
        recorder.attach(hooks)
        recorder.attach(hooks)
        registered = hooks.get_registered_hooks()
        # Each hook should still have exactly one evolution entry.
        for name in ("post_tool_call", "post_llm_call", "on_error"):
            assert registered[name].count("evolution") == 1
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_full_turn_records_trajectory(tmp_path: Path):
    recorder, store, backend = await _make_recorder(tmp_path)
    try:
        await recorder.begin_turn(
            session_key="sess1",
            chat_id="chat1",
            channel="cli",
            task_input="please do X",
            model_used="m",
        )

        # Simulate two tool calls.
        result_ok = MagicMock(success=True, error="", text="ok")
        result_bad = MagicMock(success=False, error="boom", text="")
        await recorder._on_post_tool_call(result_ok, "tool_a", {"q": 1}, _ctx("sess1"))
        await recorder._on_post_tool_call(result_bad, "tool_b", {"q": 2}, _ctx("sess1"))

        # Two LLM calls.
        await recorder._on_post_llm_call(MagicMock(content="thinking"))
        await recorder._on_post_llm_call(MagicMock(content="final"))

        traj = await recorder.end_turn(
            session_key="sess1",
            response_text="final answer",
            iteration_count=2,
        )
        assert traj is not None
        assert traj.session_id == "sess1"
        assert traj.task_input.startswith("please do X")
        assert len(traj.tools_called) == 2
        assert traj.tools_called[0].name == "tool_a"
        assert traj.tools_called[0].success is True
        assert traj.tools_called[1].success is False
        assert traj.iterations >= 2
        assert traj.skills_active == ["alpha", "beta"]

        loaded = await store.get_trajectory(traj.id)
        assert loaded is not None
        assert loaded.id == traj.id
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_end_turn_marks_failure_when_error_set(tmp_path: Path):
    recorder, _, backend = await _make_recorder(tmp_path)
    try:
        await recorder.begin_turn(
            session_key="sess",
            chat_id="c",
            channel="cli",
            task_input="t",
        )
        traj = await recorder.end_turn(
            session_key="sess",
            error="ValueError: nope",
        )
        assert traj is not None
        assert traj.outcome == "failure"
        assert "ValueError" in traj.failure_reason
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_end_turn_returns_none_when_no_active_turn(tmp_path: Path):
    recorder, _, backend = await _make_recorder(tmp_path)
    try:
        traj = await recorder.end_turn(session_key="missing")
        assert traj is None
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_post_tool_call_safe_when_session_unknown(tmp_path: Path):
    recorder, _, backend = await _make_recorder(tmp_path)
    try:
        # No begin_turn → should not raise even though there is no active turn.
        result = MagicMock(success=True, error="", text="ok")
        await recorder._on_post_tool_call(result, "x", {}, _ctx("nope"))
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_discard_turn_clears_state(tmp_path: Path):
    recorder, _, backend = await _make_recorder(tmp_path)
    try:
        await recorder.begin_turn(
            session_key="sess",
            chat_id="c",
            channel="cli",
            task_input="t",
        )
        assert await recorder.has_active("sess")
        await recorder.discard_turn("sess")
        assert not await recorder.has_active("sess")
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_reflection_invocation_is_tolerant_to_failure(tmp_path: Path):
    recorder, _, backend = await _make_recorder(tmp_path)
    try:
        # Plug in a reflection mock that always raises.
        bad_reflection = MagicMock()
        bad_reflection.critique = AsyncMock(side_effect=RuntimeError("nope"))
        recorder._reflection = bad_reflection

        await recorder.begin_turn(
            session_key="sess",
            chat_id="c",
            channel="cli",
            task_input="t",
        )
        await recorder._on_post_llm_call(MagicMock(content="hi"))
        traj = await recorder.end_turn(session_key="sess", response_text="answer")
        assert traj is not None
        # Reflection failure must not block trajectory persistence.
        assert traj.reflection_score is None
    finally:
        await backend.close()
