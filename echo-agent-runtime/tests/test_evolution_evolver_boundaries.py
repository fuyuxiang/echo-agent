"""Boundary tests for echo_agent.evolution.evolver.Evolver."""

from __future__ import annotations

from pathlib import Path
from unittest.mock import AsyncMock, MagicMock

import pytest

from echo_agent.evolution.evolver import Evolver
from echo_agent.evolution.store import TrajectoryStore
from echo_agent.evolution.types import ToolCall, Trajectory
from echo_agent.storage.sqlite import SQLiteBackend


def _llm_response(content="", tool_calls=None):
    r = MagicMock()
    r.content = content
    r.tool_calls = tool_calls or []
    r.has_tool_calls = bool(tool_calls)
    r.finish_reason = "stop"
    return r


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
        iterations=2,
        tools_called=[ToolCall(name="x", success=False)],
        failure_reason="boom",
    )
    base.update(overrides)
    return Trajectory(**base)


async def _new_store(tmp_path: Path) -> tuple[TrajectoryStore, SQLiteBackend]:
    backend = SQLiteBackend(tmp_path / "evolution.db")
    await backend.initialize()
    s = TrajectoryStore(backend)
    await s.init_schema()
    return s, backend


# ── propose() — protocol-level paths ────────────────────────────────────────


@pytest.mark.asyncio
async def test_propose_handles_llm_exception_gracefully(tmp_path: Path):
    store, backend = await _new_store(tmp_path)
    try:
        skill_store = _make_skill_store()
        provider = AsyncMock()
        provider.chat_with_retry = AsyncMock(side_effect=RuntimeError("api down"))

        evolver = Evolver(provider=provider, store=store, skill_store=skill_store)
        decision = await evolver.propose([_make_traj()], run_id="r1")
        assert decision.candidates == []
        assert decision.consumed_trajectory_ids == []
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_propose_ignores_unknown_tool_name(tmp_path: Path):
    """If the LLM calls an unknown tool the evolver replies with an error and
    keeps iterating until it gets a propose_skill or stops."""
    store, backend = await _new_store(tmp_path)
    try:
        skill_store = _make_skill_store()
        provider = AsyncMock()
        provider.chat_with_retry = AsyncMock(side_effect=[
            _llm_response(tool_calls=[_make_tc({}, name="hallucinated_tool")]),
            _llm_response(content="No more."),
        ])

        evolver = Evolver(provider=provider, store=store, skill_store=skill_store)
        decision = await evolver.propose([_make_traj()], run_id="r1")
        assert decision.candidates == []
        # Two LLM calls — first turn gets the rejection message, second turn ends.
        assert provider.chat_with_retry.await_count == 2
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_propose_parses_string_json_arguments(tmp_path: Path):
    """OpenAI-compatible providers may serialize tool args as a JSON string."""
    import json as _json
    store, backend = await _new_store(tmp_path)
    try:
        skill_store = _make_skill_store()
        provider = AsyncMock()
        json_args = _json.dumps({
            "operation": "create",
            "skill_name": "alpha",
            "content": "---\nname: alpha\ndescription: x\n---",
            "rationale": "r",
            "expected_improvement": "i",
        })
        provider.chat_with_retry = AsyncMock(side_effect=[
            _llm_response(tool_calls=[_make_tc(json_args)]),
            _llm_response(content="done"),
        ])

        evolver = Evolver(provider=provider, store=store, skill_store=skill_store, max_candidates=3)
        decision = await evolver.propose([_make_traj()], run_id="r1")
        assert len(decision.candidates) == 1
        assert decision.candidates[0].skill_name == "alpha"
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_propose_handles_malformed_json_arguments(tmp_path: Path):
    """A malformed JSON string in tool args should fall back to {} and reject the proposal."""
    store, backend = await _new_store(tmp_path)
    try:
        skill_store = _make_skill_store()
        provider = AsyncMock()
        provider.chat_with_retry = AsyncMock(side_effect=[
            _llm_response(tool_calls=[_make_tc("not-valid-json")]),
            _llm_response(content="ok"),
        ])
        evolver = Evolver(provider=provider, store=store, skill_store=skill_store)
        decision = await evolver.propose([_make_traj()], run_id="r1")
        assert decision.candidates == []
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_propose_uses_unspecified_source_trajectories_fallback(tmp_path: Path):
    """If the LLM omits source_trajectory_ids, the evolver attributes the first
    trajectories from the briefing as the source set."""
    store, backend = await _new_store(tmp_path)
    try:
        skill_store = _make_skill_store()
        provider = AsyncMock()
        provider.chat_with_retry = AsyncMock(side_effect=[
            _llm_response(tool_calls=[_make_tc({
                "operation": "create",
                "skill_name": "alpha",
                "content": "---\nname: alpha\ndescription: x\n---",
                "rationale": "r",
                "expected_improvement": "i",
            })]),
            _llm_response(content="done"),
        ])
        trajectories = [_make_traj() for _ in range(3)]
        evolver = Evolver(provider=provider, store=store, skill_store=skill_store)
        decision = await evolver.propose(trajectories, run_id="r1")
        assert len(decision.candidates) == 1
        attributed = decision.candidates[0].source_trajectories
        assert set(attributed) <= {t.id for t in trajectories}
        assert len(attributed) >= 1
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_propose_filters_out_unknown_source_ids(tmp_path: Path):
    """The LLM may invent IDs; only IDs from the briefing are accepted."""
    store, backend = await _new_store(tmp_path)
    try:
        skill_store = _make_skill_store()
        provider = AsyncMock()
        good_traj = _make_traj()
        provider.chat_with_retry = AsyncMock(side_effect=[
            _llm_response(tool_calls=[_make_tc({
                "operation": "create",
                "skill_name": "alpha",
                "content": "---\nname: alpha\ndescription: x\n---",
                "rationale": "r",
                "expected_improvement": "i",
                "source_trajectory_ids": [good_traj.id, "ghost-id"],
            })]),
            _llm_response(content="done"),
        ])
        evolver = Evolver(provider=provider, store=store, skill_store=skill_store)
        decision = await evolver.propose([good_traj], run_id="r1")
        assert len(decision.candidates) == 1
        assert decision.candidates[0].source_trajectories == [good_traj.id]
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_propose_disable_operation_succeeds(tmp_path: Path):
    """Disable proposals must produce a SkillCandidate with operation='disable'."""
    store, backend = await _new_store(tmp_path)
    try:
        skill_store = _make_skill_store(existing_names=["legacy"])
        provider = AsyncMock()
        provider.chat_with_retry = AsyncMock(side_effect=[
            _llm_response(tool_calls=[_make_tc({
                "operation": "disable",
                "skill_name": "legacy",
                "rationale": "no longer useful",
                "expected_improvement": "drop misleading skill",
            })]),
            _llm_response(content="done"),
        ])
        evolver = Evolver(provider=provider, store=store, skill_store=skill_store)
        decision = await evolver.propose([_make_traj()], run_id="r1")
        assert len(decision.candidates) == 1
        assert decision.candidates[0].operation == "disable"
        assert decision.candidates[0].parent_skill == "legacy"
    finally:
        await backend.close()


# ── _build_candidate validation gaps ───────────────────────────────────────


def test_build_candidate_unknown_operation():
    evolver = Evolver(
        provider=AsyncMock(),
        store=MagicMock(),
        skill_store=_make_skill_store(),
    )
    result, err = evolver._build_candidate(
        args={"operation": "DELETE_ALL", "skill_name": "x", "rationale": "r", "expected_improvement": "i"},
        trajectories=[_make_traj()],
        run_id="r",
    )
    assert result is None
    assert "operation must be one of" in err


def test_build_candidate_missing_skill_name():
    evolver = Evolver(
        provider=AsyncMock(), store=MagicMock(), skill_store=_make_skill_store(),
    )
    result, err = evolver._build_candidate(
        args={"operation": "create", "skill_name": "", "rationale": "r", "expected_improvement": "i"},
        trajectories=[_make_traj()], run_id="r",
    )
    assert result is None
    assert "skill_name is required" in err


def test_build_candidate_missing_rationale():
    evolver = Evolver(
        provider=AsyncMock(), store=MagicMock(), skill_store=_make_skill_store(),
    )
    result, err = evolver._build_candidate(
        args={"operation": "create", "skill_name": "alpha", "content": "x", "expected_improvement": "i"},
        trajectories=[_make_traj()], run_id="r",
    )
    assert result is None
    assert "rationale" in err


def test_build_candidate_missing_expected_improvement():
    evolver = Evolver(
        provider=AsyncMock(), store=MagicMock(), skill_store=_make_skill_store(),
    )
    result, err = evolver._build_candidate(
        args={"operation": "create", "skill_name": "alpha", "content": "x", "rationale": "r"},
        trajectories=[_make_traj()], run_id="r",
    )
    assert result is None
    assert "expected_improvement" in err


def test_build_candidate_create_missing_content():
    evolver = Evolver(
        provider=AsyncMock(), store=MagicMock(), skill_store=_make_skill_store(),
    )
    result, err = evolver._build_candidate(
        args={
            "operation": "create", "skill_name": "alpha",
            "rationale": "r", "expected_improvement": "i",
        },
        trajectories=[_make_traj()], run_id="r",
    )
    assert result is None
    assert "content is required" in err


def test_build_candidate_create_content_without_frontmatter():
    evolver = Evolver(
        provider=AsyncMock(), store=MagicMock(), skill_store=_make_skill_store(),
    )
    result, err = evolver._build_candidate(
        args={
            "operation": "create", "skill_name": "alpha",
            "content": "Just a paragraph, no YAML.",
            "rationale": "r", "expected_improvement": "i",
        },
        trajectories=[_make_traj()], run_id="r",
    )
    assert result is None
    assert "YAML frontmatter" in err


def test_build_candidate_create_content_without_description_field():
    evolver = Evolver(
        provider=AsyncMock(), store=MagicMock(), skill_store=_make_skill_store(),
    )
    result, err = evolver._build_candidate(
        args={
            "operation": "create", "skill_name": "alpha",
            "content": "---\nname: alpha\n---\n# Body",
            "rationale": "r", "expected_improvement": "i",
        },
        trajectories=[_make_traj()], run_id="r",
    )
    assert result is None
    assert "description" in err


def test_build_candidate_patch_missing_old_text():
    evolver = Evolver(
        provider=AsyncMock(), store=MagicMock(),
        skill_store=_make_skill_store(existing_names=["alpha"]),
    )
    result, err = evolver._build_candidate(
        args={
            "operation": "patch", "skill_name": "alpha",
            "patch_old": "", "patch_new": "new",
            "rationale": "r", "expected_improvement": "i",
        },
        trajectories=[_make_traj()], run_id="r",
    )
    assert result is None
    assert "patch_old" in err


def test_build_candidate_disable_missing_skill():
    evolver = Evolver(
        provider=AsyncMock(), store=MagicMock(), skill_store=_make_skill_store(),
    )
    result, err = evolver._build_candidate(
        args={
            "operation": "disable", "skill_name": "ghost",
            "rationale": "r", "expected_improvement": "i",
        },
        trajectories=[_make_traj()], run_id="r",
    )
    assert result is None
    assert "unknown skill" in err


def test_build_candidate_normalizes_uppercase_input():
    """Operation strings and skill names are normalised to lowercase."""
    evolver = Evolver(
        provider=AsyncMock(), store=MagicMock(),
        skill_store=_make_skill_store(),
    )
    result, _err = evolver._build_candidate(
        args={
            "operation": "  CREATE  ",
            "skill_name": "  ALPHA  ",
            "content": "---\nname: alpha\ndescription: x\n---",
            "rationale": "r", "expected_improvement": "i",
        },
        trajectories=[_make_traj()], run_id="r",
    )
    assert result is not None
    assert result.operation == "create"
    assert result.skill_name == "alpha"


# ── _safe_list_skills + _format_existing_skills ────────────────────────────


def test_safe_list_skills_swallows_exception():
    skill_store = MagicMock()
    skill_store.list_all.side_effect = RuntimeError("io")
    evolver = Evolver(
        provider=AsyncMock(), store=MagicMock(), skill_store=skill_store,
    )
    assert evolver._safe_list_skills() == []


def test_format_existing_skills_with_no_skills_returns_empty_string():
    evolver = Evolver(
        provider=AsyncMock(), store=MagicMock(), skill_store=_make_skill_store(),
    )
    assert evolver._format_existing_skills() == ""


def test_format_existing_skills_truncates_to_30():
    names = [f"skill-{i}" for i in range(50)]
    evolver = Evolver(
        provider=AsyncMock(), store=MagicMock(),
        skill_store=_make_skill_store(existing_names=names),
    )
    formatted = evolver._format_existing_skills()
    # 30-row cap.
    assert formatted.count("\n") + 1 == 30


# ── _build_briefing ─────────────────────────────────────────────────────────


def test_build_briefing_with_no_trajectories():
    evolver = Evolver(
        provider=AsyncMock(), store=MagicMock(), skill_store=_make_skill_store(),
    )
    out = evolver._build_briefing([])
    assert "(no trajectories provided)" in out


def test_build_briefing_renders_fields():
    evolver = Evolver(
        provider=AsyncMock(), store=MagicMock(), skill_store=_make_skill_store(),
    )
    t = _make_traj(reflection_score=0.42)
    t.reflection_suggestions = ["use cache"]
    out = evolver._build_briefing([t])
    assert "outcome=failure" in out
    assert "score=0.42" in out
    assert "use cache" in out
    assert "boom" in out  # failure_reason


# ── max_candidates cap when LLM keeps going inside same response ───────────


@pytest.mark.asyncio
async def test_propose_caps_within_a_single_response(tmp_path: Path):
    """A single LLM turn that emits 5 tool_calls must be capped at max_candidates=2."""
    store, backend = await _new_store(tmp_path)
    try:
        skill_store = _make_skill_store()
        provider = AsyncMock()
        provider.chat_with_retry = AsyncMock(return_value=_llm_response(tool_calls=[
            _make_tc({
                "operation": "create",
                "skill_name": f"alpha-{i}",
                "content": f"---\nname: alpha-{i}\ndescription: x\n---",
                "rationale": "r",
                "expected_improvement": "i",
            }, tc_id=f"tc_{i}") for i in range(5)
        ]))
        evolver = Evolver(provider=provider, store=store, skill_store=skill_store, max_candidates=2)
        decision = await evolver.propose([_make_traj()], run_id="r1")
        assert len(decision.candidates) == 2
        # We should have made exactly one LLM call — the cap kicks in immediately.
        assert provider.chat_with_retry.await_count == 1
    finally:
        await backend.close()
