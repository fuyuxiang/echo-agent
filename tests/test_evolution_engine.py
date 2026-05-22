"""Integration test for EvolutionEngine — full record → propose → gate → promote."""

from __future__ import annotations

from pathlib import Path
from unittest.mock import AsyncMock, MagicMock

import pytest

from echo_agent.config.schema import EvolutionConfig
from echo_agent.evaluation.metrics import MetricResult
from echo_agent.evaluation.runner import CaseResult, EvalReport
from echo_agent.evolution.engine import EvolutionEngine
from echo_agent.evolution.types import ToolCall, Trajectory
from echo_agent.skills.store import SkillStore
from echo_agent.storage.sqlite import SQLiteBackend


def _report(total: int, passed: int, score: float) -> EvalReport:
    cases = [
        CaseResult(
            case_id=f"c{i}",
            passed=(i < passed),
            metrics=[MetricResult(name="x", score=score, passed=True)],
        )
        for i in range(total)
    ]
    return EvalReport(results=cases, total_cases=total, passed_cases=passed)


def _llm_response(content="", tool_calls=None):
    r = MagicMock()
    r.content = content
    r.tool_calls = tool_calls or []
    r.has_tool_calls = bool(tool_calls)
    r.finish_reason = "stop"
    return r


def _propose_call(args, name="propose_skill", tc_id="tc1"):
    tc = MagicMock()
    tc.id = tc_id
    tc.name = name
    tc.arguments = args
    tc.to_openai_format.return_value = {
        "id": tc_id, "type": "function",
        "function": {"name": name, "arguments": args},
    }
    return tc


def _make_traj(outcome="failure"):
    return Trajectory(
        session_id="s",
        chat_id="c",
        channel="cli",
        task_input="please do useful thing",
        task_type="chat",
        outcome=outcome,
        iterations=2,
        tools_called=[ToolCall(name="x", args_digest="a", result_digest="r", success=outcome == "success")],
        failure_reason="" if outcome == "success" else "boom",
    )


@pytest.mark.asyncio
async def test_full_evolution_promotes_a_candidate(tmp_path: Path):
    backend = SQLiteBackend(tmp_path / "db.sqlite")
    await backend.initialize()
    try:
        skill_store = SkillStore(user_dir=tmp_path / "skills")

        provider = AsyncMock()
        provider.chat_with_retry = AsyncMock(side_effect=[
            _llm_response(tool_calls=[_propose_call({
                "operation": "create",
                "skill_name": "do-thing",
                "content": "---\nname: do-thing\ndescription: helper for chat\n---\n# Procedure\n",
                "rationale": "trajectories of type chat are failing",
                "expected_improvement": "pass_rate 0.5 → 0.9",
            })]),
            _llm_response(content="No more proposals."),
        ])

        baseline = _report(4, 2, 0.5)
        improved = _report(4, 4, 0.9)
        eval_calls = {"i": 0}

        def factory():
            runner = MagicMock()

            async def run_dataset(_):
                i = eval_calls["i"]
                eval_calls["i"] = i + 1
                return [baseline, improved][min(i, 1)]

            runner.run_dataset = run_dataset
            return runner

        dataset = MagicMock()
        dataset.cases = list(range(4))

        config = EvolutionConfig(
            enabled=True,
            trigger_mode="manual",
            max_candidates_per_run=3,
            require_strict_improvement=True,
            cooldown_seconds_after_promote=0,
            record_trajectories=False,
        )
        engine = EvolutionEngine(
            config=config,
            workspace=tmp_path,
            storage=backend,
            provider=provider,
            skill_store=skill_store,
            skill_manager=None,
            eval_runner_factory=factory,
            eval_dataset_loader=lambda: dataset,
            hooks=None,
            reflection=None,
        )

        await engine.start()
        try:
            # Seed an unconsumed trajectory.
            await engine.store.append_trajectory(_make_traj())

            run = await engine.run_evolution(trigger="manual")
            assert run.candidates_generated == 1
            assert run.candidates_promoted == 1
            assert (skill_store.user_dir / "do-thing" / "SKILL.md").exists()

            # Trajectory should now be consumed.
            assert await engine.store.count_unconsumed() == 0

            # Status summary reports state correctly.
            summary = await engine.status_summary()
            assert summary["candidates_promoted_total"] == 1
        finally:
            await engine.stop()
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_full_evolution_rejects_regression(tmp_path: Path):
    backend = SQLiteBackend(tmp_path / "db.sqlite")
    await backend.initialize()
    try:
        skill_store = SkillStore(user_dir=tmp_path / "skills")
        provider = AsyncMock()
        provider.chat_with_retry = AsyncMock(side_effect=[
            _llm_response(tool_calls=[_propose_call({
                "operation": "create",
                "skill_name": "regress-me",
                "content": "---\nname: regress-me\ndescription: harmful\n---\n",
                "rationale": "r",
                "expected_improvement": "i",
            })]),
            _llm_response(content="done"),
        ])

        baseline = _report(4, 4, 0.9)
        worse = _report(4, 1, 0.2)
        eval_calls = {"i": 0}

        def factory():
            runner = MagicMock()

            async def run_dataset(_):
                i = eval_calls["i"]
                eval_calls["i"] = i + 1
                return [baseline, worse][min(i, 1)]

            runner.run_dataset = run_dataset
            return runner

        dataset = MagicMock()
        dataset.cases = list(range(4))

        config = EvolutionConfig(
            enabled=True,
            trigger_mode="manual",
            max_candidates_per_run=3,
            require_strict_improvement=True,
            regression_threshold=0.05,
            record_trajectories=False,
            cooldown_seconds_after_promote=0,
        )
        engine = EvolutionEngine(
            config=config,
            workspace=tmp_path,
            storage=backend,
            provider=provider,
            skill_store=skill_store,
            skill_manager=None,
            eval_runner_factory=factory,
            eval_dataset_loader=lambda: dataset,
            hooks=None,
            reflection=None,
        )

        await engine.start()
        try:
            await engine.store.append_trajectory(_make_traj())
            run = await engine.run_evolution(trigger="manual")
            assert run.candidates_promoted == 0
            assert run.candidates_rejected == 1
            assert not (skill_store.user_dir / "regress-me" / "SKILL.md").exists()
        finally:
            await engine.stop()
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_run_with_no_trajectories_is_noop(tmp_path: Path):
    backend = SQLiteBackend(tmp_path / "db.sqlite")
    await backend.initialize()
    try:
        skill_store = SkillStore(user_dir=tmp_path / "skills")
        provider = AsyncMock()
        config = EvolutionConfig(enabled=True, record_trajectories=False)
        engine = EvolutionEngine(
            config=config,
            workspace=tmp_path,
            storage=backend,
            provider=provider,
            skill_store=skill_store,
            skill_manager=None,
            eval_runner_factory=lambda: MagicMock(),
            eval_dataset_loader=lambda: MagicMock(cases=[1]),
            hooks=None,
            reflection=None,
        )
        await engine.start()
        try:
            run = await engine.run_evolution(trigger="manual")
            assert run.candidates_generated == 0
            assert run.trajectories_consumed == 0
            assert "no trajectories" in run.notes
            provider.chat_with_retry.assert_not_called()
        finally:
            await engine.stop()
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_rollback_promoted_create(tmp_path: Path):
    backend = SQLiteBackend(tmp_path / "db.sqlite")
    await backend.initialize()
    try:
        skill_store = SkillStore(user_dir=tmp_path / "skills")
        provider = AsyncMock()
        provider.chat_with_retry = AsyncMock(side_effect=[
            _llm_response(tool_calls=[_propose_call({
                "operation": "create",
                "skill_name": "to-rollback",
                "content": "---\nname: to-rollback\ndescription: temp\n---\n",
                "rationale": "r",
                "expected_improvement": "i",
            })]),
            _llm_response(content="done"),
        ])
        baseline = _report(2, 1, 0.5)
        improved = _report(2, 2, 0.9)
        calls = {"i": 0}

        def factory():
            runner = MagicMock()

            async def run_dataset(_):
                i = calls["i"]
                calls["i"] = i + 1
                return [baseline, improved][min(i, 1)]

            runner.run_dataset = run_dataset
            return runner

        config = EvolutionConfig(
            enabled=True, record_trajectories=False, cooldown_seconds_after_promote=0,
        )
        engine = EvolutionEngine(
            config=config,
            workspace=tmp_path,
            storage=backend,
            provider=provider,
            skill_store=skill_store,
            skill_manager=None,
            eval_runner_factory=factory,
            eval_dataset_loader=lambda: MagicMock(cases=list(range(2))),
            hooks=None,
            reflection=None,
        )
        await engine.start()
        try:
            await engine.store.append_trajectory(_make_traj())
            await engine.run_evolution(trigger="manual")
            assert (skill_store.user_dir / "to-rollback" / "SKILL.md").exists()

            ok, msg = await engine.rollback_skill("to-rollback")
            assert ok, msg
            assert not (skill_store.user_dir / "to-rollback" / "SKILL.md").exists()

            promoted_again = await engine.store.latest_promoted_for_skill("to-rollback")
            # No promoted record should remain after rollback.
            assert promoted_again is None
        finally:
            await engine.stop()
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_disabled_engine_is_inert(tmp_path: Path):
    """When ``record_trajectories=False`` and no hooks attached, the recorder is dormant."""
    backend = SQLiteBackend(tmp_path / "db.sqlite")
    await backend.initialize()
    try:
        skill_store = SkillStore(user_dir=tmp_path / "skills")
        provider = AsyncMock()
        config = EvolutionConfig(enabled=True, record_trajectories=False)
        engine = EvolutionEngine(
            config=config,
            workspace=tmp_path,
            storage=backend,
            provider=provider,
            skill_store=skill_store,
            skill_manager=None,
            eval_runner_factory=lambda: MagicMock(),
            eval_dataset_loader=lambda: MagicMock(cases=[1]),
            hooks=None,
            reflection=None,
        )
        await engine.start()
        try:
            # No trajectories were recorded, so begin/end_turn should be no-ops on direct use.
            assert not await engine.recorder.has_active("anything")
        finally:
            await engine.stop()
    finally:
        await backend.close()
