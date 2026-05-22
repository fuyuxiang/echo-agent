"""Boundary tests for echo_agent.evolution.recorder.TrajectoryRecorder."""

from __future__ import annotations

from pathlib import Path
from unittest.mock import AsyncMock, MagicMock

import pytest

from echo_agent.evolution.recorder import TrajectoryRecorder
from echo_agent.evolution.store import TrajectoryStore
from echo_agent.evolution.types import ToolCall
from echo_agent.plugins.hooks import HookRegistry
from echo_agent.storage.sqlite import SQLiteBackend


async def _make_recorder(
    tmp_path: Path,
    *,
    redact: bool = True,
    skill_store=None,
    reflection=None,
):
    backend = SQLiteBackend(tmp_path / "evolution.db")
    await backend.initialize()
    store = TrajectoryStore(backend)
    await store.init_schema()
    if skill_store is None:
        skill_store = MagicMock()
        skill_store.list_all.return_value = [
            type("M", (), {"name": "alpha"})(),
        ]
    recorder = TrajectoryRecorder(
        store, redact_args=redact, skill_store=skill_store, reflection=reflection,
    )
    return recorder, store, backend


def _ctx(session_key: str):
    ctx = MagicMock()
    ctx.session_key = session_key
    return ctx


# ── attach / detach ──────────────────────────────────────────────────────────


@pytest.mark.asyncio
async def test_detach_removes_all_evolution_hooks(tmp_path: Path):
    recorder, _, backend = await _make_recorder(tmp_path)
    try:
        hooks = HookRegistry()
        recorder.attach(hooks)
        recorder.detach(hooks)
        registered = hooks.get_registered_hooks()
        for name in ("post_tool_call", "post_llm_call", "on_error"):
            assert name not in registered or "evolution" not in registered[name]
    finally:
        await backend.close()


# ── begin_turn boundary ──────────────────────────────────────────────────────


@pytest.mark.asyncio
async def test_begin_turn_with_empty_session_key_is_noop(tmp_path: Path):
    recorder, _, backend = await _make_recorder(tmp_path)
    try:
        await recorder.begin_turn(
            session_key="", chat_id="c", channel="cli", task_input="hi",
        )
        # Recorder should have no active state.
        assert not await recorder.has_active("")
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_begin_turn_truncates_long_input(tmp_path: Path):
    recorder, store, backend = await _make_recorder(tmp_path)
    recorder._max_input_chars = 50
    try:
        long_input = "x" * 1000
        await recorder.begin_turn(
            session_key="sess", chat_id="c", channel="cli", task_input=long_input,
        )
        traj = await recorder.end_turn(session_key="sess", response_text="ok")
        assert traj is not None
        assert len(traj.task_input) == 50
    finally:
        await backend.close()


# ── end_turn outcome inference ──────────────────────────────────────────────


@pytest.mark.asyncio
async def test_end_turn_partial_when_no_response_and_no_content(tmp_path: Path):
    recorder, _, backend = await _make_recorder(tmp_path)
    try:
        await recorder.begin_turn(session_key="s", chat_id="c", channel="cli", task_input="t")
        traj = await recorder.end_turn(session_key="s", response_text="")
        assert traj is not None
        assert traj.outcome == "partial"
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_end_turn_partial_when_majority_tools_failed(tmp_path: Path):
    recorder, _, backend = await _make_recorder(tmp_path)
    try:
        await recorder.begin_turn(session_key="s", chat_id="c", channel="cli", task_input="t")
        # Manually inject mostly-failed tool calls.
        recorder._active["s"].tools_called = [
            ToolCall(name="a", success=False),
            ToolCall(name="b", success=False),
            ToolCall(name="c", success=True),
        ]
        traj = await recorder.end_turn(session_key="s", response_text="something")
        assert traj is not None
        assert traj.outcome == "partial"
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_end_turn_explicit_outcome_override(tmp_path: Path):
    recorder, _, backend = await _make_recorder(tmp_path)
    try:
        await recorder.begin_turn(session_key="s", chat_id="c", channel="cli", task_input="t")
        traj = await recorder.end_turn(
            session_key="s",
            response_text="ok",
            outcome="failure",  # explicit override should win
        )
        assert traj is not None
        assert traj.outcome == "failure"
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_end_turn_iteration_count_takes_max_with_existing(tmp_path: Path):
    recorder, _, backend = await _make_recorder(tmp_path)
    try:
        await recorder.begin_turn(session_key="s", chat_id="c", channel="cli", task_input="t")
        # Bump from llm hook three times.
        for _ in range(3):
            await recorder._on_post_llm_call(MagicMock(content="step"))
        traj = await recorder.end_turn(session_key="s", iteration_count=2, response_text="ok")
        # Should keep the higher of the two (3 from hook > 2 explicit).
        assert traj.iterations == 3
    finally:
        await backend.close()


# ── persistence error ──────────────────────────────────────────────────────


@pytest.mark.asyncio
async def test_end_turn_returns_none_on_store_failure(tmp_path: Path):
    recorder, store, backend = await _make_recorder(tmp_path)
    try:
        store.append_trajectory = AsyncMock(side_effect=RuntimeError("db locked"))
        await recorder.begin_turn(session_key="s", chat_id="c", channel="cli", task_input="t")
        result = await recorder.end_turn(session_key="s", response_text="ok")
        assert result is None
    finally:
        await backend.close()


# ── Reflection happy path ───────────────────────────────────────────────────


@pytest.mark.asyncio
async def test_reflection_populates_score_and_suggestions(tmp_path: Path):
    feedback = MagicMock()
    feedback.score = 0.83
    feedback.critique = "could be tighter"
    feedback.suggestions = ["use cached weather", "skip retry"]

    reflection = MagicMock()
    reflection.critique = AsyncMock(return_value=feedback)

    recorder, _, backend = await _make_recorder(tmp_path, reflection=reflection)
    try:
        await recorder.begin_turn(session_key="s", chat_id="c", channel="cli", task_input="t")
        await recorder._on_post_llm_call(MagicMock(content="ans"))
        traj = await recorder.end_turn(session_key="s", response_text="answer")
        assert traj is not None
        assert traj.reflection_score == pytest.approx(0.83)
        assert traj.reflection_critique == "could be tighter"
        assert traj.reflection_suggestions == ["use cached weather", "skip retry"]
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_reflection_skipped_when_error_set(tmp_path: Path):
    """Reflection must not run when end_turn carries an error."""
    reflection = MagicMock()
    reflection.critique = AsyncMock()
    recorder, _, backend = await _make_recorder(tmp_path, reflection=reflection)
    try:
        await recorder.begin_turn(session_key="s", chat_id="c", channel="cli", task_input="t")
        await recorder._on_post_llm_call(MagicMock(content="x"))
        await recorder.end_turn(session_key="s", error="boom")
        reflection.critique.assert_not_called()
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_reflection_skipped_when_no_iterations(tmp_path: Path):
    """Without LLM iterations, reflection has nothing to critique."""
    reflection = MagicMock()
    reflection.critique = AsyncMock()
    recorder, _, backend = await _make_recorder(tmp_path, reflection=reflection)
    try:
        await recorder.begin_turn(session_key="s", chat_id="c", channel="cli", task_input="t")
        await recorder.end_turn(session_key="s", response_text="ok")
        reflection.critique.assert_not_called()
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_reflection_truncates_long_critique_and_suggestions(tmp_path: Path):
    feedback = MagicMock()
    feedback.score = 0.5
    feedback.critique = "x" * 5000
    feedback.suggestions = ["y" * 1000] * 10  # 10 entries, each 1000 chars

    reflection = MagicMock()
    reflection.critique = AsyncMock(return_value=feedback)
    recorder, _, backend = await _make_recorder(tmp_path, reflection=reflection)
    try:
        await recorder.begin_turn(session_key="s", chat_id="c", channel="cli", task_input="t")
        await recorder._on_post_llm_call(MagicMock(content="x"))
        traj = await recorder.end_turn(session_key="s", response_text="ans")
        assert traj is not None
        assert len(traj.reflection_critique) == 1000  # capped
        assert len(traj.reflection_suggestions) == 5  # capped at 5
        assert all(len(s) == 300 for s in traj.reflection_suggestions)
    finally:
        await backend.close()


# ── _snapshot_active_skills ─────────────────────────────────────────────────


def test_snapshot_active_skills_returns_empty_when_no_store(tmp_path: Path):
    recorder = TrajectoryRecorder(
        store=MagicMock(),
        skill_store=None,
    )
    assert recorder._snapshot_active_skills() == []


def test_snapshot_active_skills_swallows_exception(tmp_path: Path):
    skill_store = MagicMock()
    skill_store.list_all.side_effect = RuntimeError("fs error")
    recorder = TrajectoryRecorder(store=MagicMock(), skill_store=skill_store)
    assert recorder._snapshot_active_skills() == []


# ── _on_post_tool_call edge cases ───────────────────────────────────────────


@pytest.mark.asyncio
async def test_post_tool_call_without_ctx(tmp_path: Path):
    recorder, _, backend = await _make_recorder(tmp_path)
    try:
        result = MagicMock(success=True, error="", text="ok")
        await recorder._on_post_tool_call(result, "x", {}, None)
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_post_tool_call_empty_session_key(tmp_path: Path):
    recorder, _, backend = await _make_recorder(tmp_path)
    try:
        result = MagicMock(success=True, error="", text="ok")
        await recorder._on_post_tool_call(result, "x", {}, _ctx(""))
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_post_tool_call_redact_off_path(tmp_path: Path):
    recorder, _, backend = await _make_recorder(tmp_path, redact=False)
    try:
        await recorder.begin_turn(session_key="s", chat_id="c", channel="cli", task_input="t")
        result = MagicMock(success=True, error="", text="result-payload-here")
        await recorder._on_post_tool_call(
            result, "tool_x", {"secret": "value"}, _ctx("s"),
        )
        traj = await recorder.end_turn(session_key="s", response_text="ok")
        assert traj is not None
        # Without redaction, the args show up verbatim (truncated to 500 chars).
        tc = traj.tools_called[0]
        assert "secret" in tc.args_digest
        assert "result-payload-here" in tc.result_digest
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_post_tool_call_swallows_unexpected_exception(tmp_path: Path):
    recorder, _, backend = await _make_recorder(tmp_path)
    try:
        # Corrupt the lock so accessing it raises.
        broken_result = MagicMock()
        type(broken_result).text = property(lambda self: (_ for _ in ()).throw(RuntimeError("evil")))
        await recorder.begin_turn(session_key="s", chat_id="c", channel="cli", task_input="t")
        # Should not raise.
        await recorder._on_post_tool_call(broken_result, "x", {}, _ctx("s"))
    finally:
        await backend.close()


# ── _on_post_llm_call edge cases ───────────────────────────────────────────


@pytest.mark.asyncio
async def test_post_llm_call_without_active_turn_is_safe(tmp_path: Path):
    recorder, _, backend = await _make_recorder(tmp_path)
    try:
        await recorder._on_post_llm_call(MagicMock(content="x"))
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_post_llm_call_swallows_unexpected_exception(tmp_path: Path):
    recorder, _, backend = await _make_recorder(tmp_path)
    try:
        broken = MagicMock()
        type(broken).content = property(lambda self: (_ for _ in ()).throw(RuntimeError("nope")))
        await recorder._on_post_llm_call(broken)
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_on_error_callback_is_quiet(tmp_path: Path):
    """The on_error hook is purely defensive and must always return None."""
    recorder, _, backend = await _make_recorder(tmp_path)
    try:
        result = await recorder._on_error("anything", error="boom")
        assert result is None
    finally:
        await backend.close()


# ── _safe_reflect plan-construction failure ────────────────────────────────


@pytest.mark.asyncio
async def test_safe_reflect_when_reflection_is_none_returns_none(tmp_path: Path):
    recorder, _, backend = await _make_recorder(tmp_path, reflection=None)
    try:
        from echo_agent.evolution.types import Trajectory
        result = await recorder._safe_reflect(Trajectory(task_input="x"))
        assert result is None
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_safe_reflect_swallows_critique_exception(tmp_path: Path):
    reflection = MagicMock()
    reflection.critique = AsyncMock(side_effect=RuntimeError("rate limited"))
    recorder, _, backend = await _make_recorder(tmp_path, reflection=reflection)
    try:
        from echo_agent.evolution.types import Trajectory
        result = await recorder._safe_reflect(Trajectory(task_input="x"))
        assert result is None
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_reflection_branch_swallows_safe_reflect_exception(tmp_path: Path):
    """end_turn must keep going if _safe_reflect itself raises (defence in depth)."""
    reflection = MagicMock()
    recorder, _, backend = await _make_recorder(tmp_path, reflection=reflection)
    try:
        recorder._safe_reflect = AsyncMock(side_effect=RuntimeError("unexpected"))
        await recorder.begin_turn(session_key="s", chat_id="c", channel="cli", task_input="t")
        await recorder._on_post_llm_call(MagicMock(content="x"))
        traj = await recorder.end_turn(session_key="s", response_text="ans")
        assert traj is not None
        # No reflection score set because the helper exploded.
        assert traj.reflection_score is None
    finally:
        await backend.close()
