"""Tests for echo_agent.evolution.types — Trajectory / SkillCandidate / EvolutionRun."""

from __future__ import annotations

from echo_agent.evolution.types import (
    EvolutionRun,
    SkillCandidate,
    ToolCall,
    Trajectory,
    digest,
)


def test_digest_truncates_long_input():
    long = "x" * 1000
    out = digest(long, max_chars=50)
    assert out.startswith("x" * 50)
    assert "…" in out
    assert "[" in out and out.endswith("]")


def test_digest_handles_dict_and_list():
    out_dict = digest({"a": 1, "b": [2, 3]})
    out_list = digest([1, 2, 3])
    assert out_dict.endswith("]")
    assert out_list.endswith("]")


def test_digest_is_deterministic_for_same_input():
    a = digest({"a": 1})
    b = digest({"a": 1})
    assert a == b


def test_digest_differs_for_different_inputs():
    assert digest("foo") != digest("bar")


def test_trajectory_round_trip():
    t = Trajectory(
        session_id="sess",
        chat_id="chat",
        channel="cli",
        task_input="hello",
        task_type="chat",
        tools_called=[
            ToolCall(name="x", args_digest="a", result_digest="r", success=True),
        ],
        iterations=3,
        outcome="success",
        reflection_score=0.8,
        reflection_suggestions=["faster"],
    )
    data = t.to_dict()
    rebuilt = Trajectory.from_dict(data)
    assert rebuilt.session_id == "sess"
    assert rebuilt.iterations == 3
    assert rebuilt.outcome == "success"
    assert rebuilt.reflection_score == 0.8
    assert rebuilt.tools_called[0].name == "x"
    assert rebuilt.reflection_suggestions == ["faster"]


def test_skill_candidate_round_trip():
    c = SkillCandidate(
        operation="patch",
        skill_name="my-skill",
        proposed_patch_old="foo",
        proposed_patch_new="bar",
        rationale="r",
        expected_improvement="i",
        source_trajectories=["t1"],
    )
    data = c.to_dict()
    rebuilt = SkillCandidate.from_dict(data)
    assert rebuilt.skill_name == "my-skill"
    assert rebuilt.operation == "patch"
    assert rebuilt.proposed_patch_old == "foo"
    assert rebuilt.proposed_patch_new == "bar"
    assert rebuilt.source_trajectories == ["t1"]
    assert rebuilt.status == "pending"


def test_evolution_run_round_trip():
    r = EvolutionRun(
        triggered_by="manual",
        trajectories_consumed=10,
        candidates_generated=2,
        candidates_promoted=1,
        candidates_rejected=1,
    )
    rebuilt = EvolutionRun.from_dict(r.to_dict())
    assert rebuilt.triggered_by == "manual"
    assert rebuilt.trajectories_consumed == 10
    assert rebuilt.candidates_promoted == 1


def test_tool_call_defaults():
    tc = ToolCall(name="x")
    assert tc.success is True
    assert tc.error == ""
    rebuilt = ToolCall.from_dict(tc.to_dict())
    assert rebuilt.name == "x"
