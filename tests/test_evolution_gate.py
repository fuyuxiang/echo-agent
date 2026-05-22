"""Tests for echo_agent.evolution.gate — PromotionGate."""

from __future__ import annotations

from pathlib import Path
from unittest.mock import MagicMock

import pytest

from echo_agent.evaluation.runner import EvalReport, CaseResult
from echo_agent.evolution.gate import PromotionGate
from echo_agent.evolution.store import TrajectoryStore
from echo_agent.evolution.types import SkillCandidate
from echo_agent.skills.store import SkillStore
from echo_agent.storage.sqlite import SQLiteBackend


def _make_report(*, total: int, passed: int, avg_score: float) -> EvalReport:
    """Construct a real EvalReport with the same shape EvalRunner produces."""
    cases = [CaseResult(case_id=f"c{i}", passed=(i < passed)) for i in range(total)]
    return EvalReport(results=cases, total_cases=total, passed_cases=passed, duration_ms=10.0)


def _force_avg_score(report: EvalReport, score: float) -> EvalReport:
    """EvalReport.avg_score reads from r.score; we cheat per case."""
    for r in report.results:
        from echo_agent.evaluation.metrics import MetricResult
        r.metrics = [MetricResult(name="x", score=score, passed=True)]
    return report


def _factory_returning(reports: list[EvalReport]):
    calls = {"i": 0}

    def factory():
        runner = MagicMock()

        async def run_dataset(_dataset):
            i = calls["i"]
            calls["i"] = i + 1
            return reports[min(i, len(reports) - 1)]

        runner.run_dataset = run_dataset
        return runner

    return factory, calls


def _dataset_loader(case_count: int = 3):
    dataset = MagicMock()
    dataset.cases = list(range(case_count))
    return lambda: dataset


async def _new_setup(tmp_path: Path) -> tuple[TrajectoryStore, SkillStore, SQLiteBackend]:
    backend = SQLiteBackend(tmp_path / "evolution.db")
    await backend.initialize()
    store = TrajectoryStore(backend)
    await store.init_schema()
    user_dir = tmp_path / "skills"
    user_dir.mkdir()
    skill_store = SkillStore(user_dir=user_dir)
    return store, skill_store, backend


@pytest.mark.asyncio
async def test_promotes_strict_improvement(tmp_path: Path):
    store, skill_store, backend = await _new_setup(tmp_path)
    try:
        baseline = _force_avg_score(_make_report(total=4, passed=2, avg_score=0.5), 0.5)
        improved = _force_avg_score(_make_report(total=4, passed=4, avg_score=0.9), 0.9)
        factory, _ = _factory_returning([baseline, improved])

        gate = PromotionGate(
            eval_runner_factory=factory,
            eval_dataset_loader=_dataset_loader(),
            skill_store=skill_store,
            skill_manager=None,
            store=store,
            require_strict_improvement=True,
        )
        candidate = SkillCandidate(
            operation="create",
            skill_name="alpha",
            proposed_content=(
                "---\nname: alpha\ndescription: do X\n---\n# Alpha\n"
            ),
            rationale="r",
            expected_improvement="i",
        )
        await store.save_candidate(candidate)

        decision = await gate.evaluate(candidate)
        assert decision.promoted is True
        # Refresh
        refreshed = await store.get_candidate(candidate.id)
        assert refreshed.status == "promoted"
        # Skill exists on disk
        assert (skill_store.user_dir / "alpha" / "SKILL.md").exists()
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_rejects_regression_and_restores(tmp_path: Path):
    store, skill_store, backend = await _new_setup(tmp_path)
    try:
        baseline = _force_avg_score(_make_report(total=4, passed=4, avg_score=0.9), 0.9)
        worse = _force_avg_score(_make_report(total=4, passed=1, avg_score=0.2), 0.2)
        factory, _ = _factory_returning([baseline, worse])

        gate = PromotionGate(
            eval_runner_factory=factory,
            eval_dataset_loader=_dataset_loader(),
            skill_store=skill_store,
            skill_manager=None,
            store=store,
            regression_threshold=0.05,
            require_strict_improvement=True,
        )
        candidate = SkillCandidate(
            operation="create",
            skill_name="alpha",
            proposed_content=(
                "---\nname: alpha\ndescription: y\n---\n"
            ),
            rationale="r",
            expected_improvement="i",
        )
        await store.save_candidate(candidate)

        decision = await gate.evaluate(candidate)
        assert decision.promoted is False
        assert "regression" in decision.reason
        refreshed = await store.get_candidate(candidate.id)
        assert refreshed.status == "rejected"
        # Skill must have been restored — it should not exist on disk.
        assert not (skill_store.user_dir / "alpha" / "SKILL.md").exists()
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_rejects_tie_under_strict_mode(tmp_path: Path):
    store, skill_store, backend = await _new_setup(tmp_path)
    try:
        baseline = _force_avg_score(_make_report(total=4, passed=2, avg_score=0.5), 0.5)
        same = _force_avg_score(_make_report(total=4, passed=2, avg_score=0.5), 0.5)
        factory, _ = _factory_returning([baseline, same])

        gate = PromotionGate(
            eval_runner_factory=factory,
            eval_dataset_loader=_dataset_loader(),
            skill_store=skill_store,
            skill_manager=None,
            store=store,
            require_strict_improvement=True,
        )
        candidate = SkillCandidate(
            operation="create",
            skill_name="alpha",
            proposed_content="---\nname: alpha\ndescription: x\n---\n",
        )
        await store.save_candidate(candidate)
        decision = await gate.evaluate(candidate)
        assert decision.promoted is False
        assert "no strict improvement" in decision.reason
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_promotes_tie_under_loose_mode(tmp_path: Path):
    store, skill_store, backend = await _new_setup(tmp_path)
    try:
        baseline = _force_avg_score(_make_report(total=4, passed=2, avg_score=0.5), 0.5)
        same = _force_avg_score(_make_report(total=4, passed=2, avg_score=0.5), 0.5)
        factory, _ = _factory_returning([baseline, same])

        gate = PromotionGate(
            eval_runner_factory=factory,
            eval_dataset_loader=_dataset_loader(),
            skill_store=skill_store,
            skill_manager=None,
            store=store,
            require_strict_improvement=False,
        )
        candidate = SkillCandidate(
            operation="create",
            skill_name="alpha",
            proposed_content="---\nname: alpha\ndescription: x\n---\n",
        )
        await store.save_candidate(candidate)
        decision = await gate.evaluate(candidate)
        assert decision.promoted is True
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_empty_dataset_rejects_immediately(tmp_path: Path):
    store, skill_store, backend = await _new_setup(tmp_path)
    try:
        empty_dataset = MagicMock()
        empty_dataset.cases = []
        factory, _ = _factory_returning([_make_report(total=0, passed=0, avg_score=0.0)])
        gate = PromotionGate(
            eval_runner_factory=factory,
            eval_dataset_loader=lambda: empty_dataset,
            skill_store=skill_store,
            skill_manager=None,
            store=store,
        )
        candidate = SkillCandidate(operation="create", skill_name="alpha")
        await store.save_candidate(candidate)
        decision = await gate.evaluate(candidate)
        assert decision.promoted is False
        assert "empty" in decision.reason or "missing" in decision.reason
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_review_required_holds_promotion(tmp_path: Path):
    store, skill_store, backend = await _new_setup(tmp_path)
    try:
        baseline = _force_avg_score(_make_report(total=4, passed=2, avg_score=0.5), 0.5)
        improved = _force_avg_score(_make_report(total=4, passed=4, avg_score=0.9), 0.9)
        factory, _ = _factory_returning([baseline, improved])

        gate = PromotionGate(
            eval_runner_factory=factory,
            eval_dataset_loader=_dataset_loader(),
            skill_store=skill_store,
            skill_manager=None,
            store=store,
            candidate_review_required=True,
        )
        candidate = SkillCandidate(
            operation="create",
            skill_name="alpha",
            proposed_content="---\nname: alpha\ndescription: x\n---\n",
        )
        await store.save_candidate(candidate)
        decision = await gate.evaluate(candidate)
        assert decision.promoted is False
        refreshed = await store.get_candidate(candidate.id)
        assert refreshed.status == "needs_review"
        assert not (skill_store.user_dir / "alpha" / "SKILL.md").exists()
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_apply_failure_rejects_without_eval(tmp_path: Path):
    store, skill_store, backend = await _new_setup(tmp_path)
    try:
        baseline = _force_avg_score(_make_report(total=2, passed=2, avg_score=1.0), 1.0)

        # The factory returns one report that we never get to use a second time.
        async def never_called(_):
            raise AssertionError("eval should not have run twice")

        runner_factory_calls = {"n": 0}

        def factory():
            runner = MagicMock()

            async def run_dataset(_dataset):
                runner_factory_calls["n"] += 1
                return baseline

            runner.run_dataset = run_dataset
            return runner

        gate = PromotionGate(
            eval_runner_factory=factory,
            eval_dataset_loader=_dataset_loader(),
            skill_store=skill_store,
            skill_manager=None,
            store=store,
        )
        # Patch SkillCandidate to be unbuildable: missing description in frontmatter triggers create_skill error.
        candidate = SkillCandidate(
            operation="create",
            skill_name="alpha",
            proposed_content="not yaml at all",
        )
        await store.save_candidate(candidate)
        decision = await gate.evaluate(candidate)
        assert decision.promoted is False
        # Only the baseline eval should have run.
        assert runner_factory_calls["n"] == 1
    finally:
        await backend.close()
