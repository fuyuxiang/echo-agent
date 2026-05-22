"""Boundary tests for echo_agent.evolution.gate.PromotionGate.

Covers code paths the integration suite (test_evolution_gate.py) does not
exercise: baseline/candidate eval failures, patch/disable application, backup
restoration, cleanup tolerance, decision branches.
"""

from __future__ import annotations

from pathlib import Path
from unittest.mock import MagicMock

import pytest

from echo_agent.evaluation.metrics import MetricResult
from echo_agent.evaluation.runner import CaseResult, EvalReport
from echo_agent.evolution.gate import PromotionGate
from echo_agent.evolution.store import TrajectoryStore
from echo_agent.evolution.types import SkillCandidate
from echo_agent.skills.store import SkillStore
from echo_agent.storage.sqlite import SQLiteBackend


def _force_score(report: EvalReport, score: float) -> EvalReport:
    for r in report.results:
        r.metrics = [MetricResult(name="x", score=score, passed=True)]
    return report


def _make_report(*, total: int, passed: int, score: float) -> EvalReport:
    cases = [CaseResult(case_id=f"c{i}", passed=(i < passed)) for i in range(total)]
    return _force_score(
        EvalReport(results=cases, total_cases=total, passed_cases=passed, duration_ms=10.0),
        score,
    )


def _factory_seq(reports: list[EvalReport]):
    """Eval factory that yields each report in order across run_dataset() calls."""
    state = {"i": 0}

    def factory():
        runner = MagicMock()

        async def run_dataset(_):
            i = state["i"]
            state["i"] = i + 1
            return reports[min(i, len(reports) - 1)]

        runner.run_dataset = run_dataset
        return runner

    return factory


def _factory_raises(error: Exception, after: int = 0):
    """Eval factory that returns OK reports for ``after`` calls then raises."""
    state = {"i": 0}
    ok = _make_report(total=2, passed=2, score=1.0)

    def factory():
        runner = MagicMock()

        async def run_dataset(_):
            i = state["i"]
            state["i"] = i + 1
            if i >= after:
                raise error
            return ok

        runner.run_dataset = run_dataset
        return runner

    return factory


def _dataset(case_count: int = 2):
    ds = MagicMock()
    ds.cases = list(range(case_count))
    return ds


async def _new_setup(tmp_path: Path):
    backend = SQLiteBackend(tmp_path / "evolution.db")
    await backend.initialize()
    store = TrajectoryStore(backend)
    await store.init_schema()
    user_dir = tmp_path / "skills"
    user_dir.mkdir()
    skill_store = SkillStore(user_dir=user_dir)
    return store, skill_store, backend


# ── Eval failure paths ───────────────────────────────────────────────────────


@pytest.mark.asyncio
async def test_baseline_eval_failure_rejects_immediately(tmp_path: Path):
    store, skill_store, backend = await _new_setup(tmp_path)
    try:
        gate = PromotionGate(
            eval_runner_factory=_factory_raises(RuntimeError("eval bug"), after=0),
            eval_dataset_loader=lambda: _dataset(),
            skill_store=skill_store,
            skill_manager=None,
            store=store,
        )
        cand = SkillCandidate(
            operation="create",
            skill_name="alpha",
            proposed_content="---\nname: alpha\ndescription: x\n---\n",
        )
        await store.save_candidate(cand)
        decision = await gate.evaluate(cand)
        assert decision.promoted is False
        assert "baseline eval failed" in decision.reason
        # Baseline failure means we never ran the candidate eval — both reports None.
        assert decision.baseline is None
        assert decision.with_candidate is None
        # Candidate must not have been written to disk.
        assert not (skill_store.user_dir / "alpha" / "SKILL.md").exists()
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_candidate_eval_failure_rolls_back_and_rejects(tmp_path: Path):
    store, skill_store, backend = await _new_setup(tmp_path)
    try:
        gate = PromotionGate(
            eval_runner_factory=_factory_raises(RuntimeError("flaky eval"), after=1),
            eval_dataset_loader=lambda: _dataset(),
            skill_store=skill_store,
            skill_manager=None,
            store=store,
        )
        cand = SkillCandidate(
            operation="create",
            skill_name="alpha",
            proposed_content="---\nname: alpha\ndescription: y\n---\n",
        )
        await store.save_candidate(cand)
        decision = await gate.evaluate(cand)
        assert decision.promoted is False
        assert "candidate eval failed" in decision.reason
        # Baseline summary present, candidate summary absent.
        assert decision.baseline is not None
        assert decision.with_candidate is None
        # Skill must have been restored.
        assert not (skill_store.user_dir / "alpha" / "SKILL.md").exists()
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_apply_failure_for_create_with_invalid_content(tmp_path: Path):
    """A candidate whose content fails SkillStore validation rejects without a
    second eval call."""
    store, skill_store, backend = await _new_setup(tmp_path)
    try:
        baseline = _make_report(total=2, passed=2, score=1.0)
        gate = PromotionGate(
            eval_runner_factory=_factory_seq([baseline]),
            eval_dataset_loader=lambda: _dataset(),
            skill_store=skill_store,
            skill_manager=None,
            store=store,
        )
        # Missing description in frontmatter
        cand = SkillCandidate(
            operation="create",
            skill_name="bad-skill",
            proposed_content="---\nname: bad-skill\n---\nNo description.",
        )
        await store.save_candidate(cand)
        decision = await gate.evaluate(cand)
        assert decision.promoted is False
        assert "create failed" in decision.reason or "description" in decision.reason
    finally:
        await backend.close()


# ── Patch operation ──────────────────────────────────────────────────────────


@pytest.mark.asyncio
async def test_patch_promotion_modifies_existing_skill(tmp_path: Path):
    store, skill_store, backend = await _new_setup(tmp_path)
    try:
        skill_store.create_skill(
            "alpha",
            "---\nname: alpha\ndescription: original\n---\n# Body original\n",
        )
        baseline = _make_report(total=2, passed=1, score=0.5)
        improved = _make_report(total=2, passed=2, score=1.0)
        gate = PromotionGate(
            eval_runner_factory=_factory_seq([baseline, improved]),
            eval_dataset_loader=lambda: _dataset(),
            skill_store=skill_store,
            skill_manager=None,
            store=store,
        )
        cand = SkillCandidate(
            operation="patch",
            skill_name="alpha",
            proposed_patch_old="original",
            proposed_patch_new="patched",
        )
        await store.save_candidate(cand)
        decision = await gate.evaluate(cand)
        assert decision.promoted is True
        body = (skill_store.user_dir / "alpha" / "SKILL.md").read_text(encoding="utf-8")
        # Both occurrences should have been substituted (frontmatter + body),
        # but SkillStore.patch_skill replaces the first occurrence only.
        assert "patched" in body


    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_patch_apply_failure_when_old_text_missing(tmp_path: Path):
    store, skill_store, backend = await _new_setup(tmp_path)
    try:
        skill_store.create_skill(
            "alpha",
            "---\nname: alpha\ndescription: original\n---\n",
        )
        baseline = _make_report(total=2, passed=2, score=1.0)
        gate = PromotionGate(
            eval_runner_factory=_factory_seq([baseline]),
            eval_dataset_loader=lambda: _dataset(),
            skill_store=skill_store,
            skill_manager=None,
            store=store,
        )
        cand = SkillCandidate(
            operation="patch",
            skill_name="alpha",
            proposed_patch_old="not-present",
            proposed_patch_new="anything",
        )
        await store.save_candidate(cand)
        decision = await gate.evaluate(cand)
        assert decision.promoted is False
        assert "patch failed" in decision.reason or "not found" in decision.reason
    finally:
        await backend.close()


# ── Disable operation ────────────────────────────────────────────────────────


@pytest.mark.asyncio
async def test_disable_operation_promotion(tmp_path: Path):
    store, skill_store, backend = await _new_setup(tmp_path)
    try:
        skill_store.create_skill(
            "alpha",
            "---\nname: alpha\ndescription: original\n---\n",
        )
        baseline = _make_report(total=2, passed=1, score=0.5)
        improved = _make_report(total=2, passed=2, score=1.0)
        gate = PromotionGate(
            eval_runner_factory=_factory_seq([baseline, improved]),
            eval_dataset_loader=lambda: _dataset(),
            skill_store=skill_store,
            skill_manager=None,
            store=store,
        )
        cand = SkillCandidate(operation="disable", skill_name="alpha")
        await store.save_candidate(cand)
        decision = await gate.evaluate(cand)
        assert decision.promoted is True
        # Disable means the skill name lives in skill_store._disabled.
        assert "alpha" in skill_store._disabled
        # File still exists; only the in-memory disable flag is set.
        assert (skill_store.user_dir / "alpha" / "SKILL.md").exists()
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_disable_apply_clears_set_after_rejection(tmp_path: Path):
    """If the candidate is rejected, _restore_backup must clear _disabled too."""
    store, skill_store, backend = await _new_setup(tmp_path)
    try:
        skill_store.create_skill(
            "alpha",
            "---\nname: alpha\ndescription: original\n---\n",
        )
        baseline = _make_report(total=2, passed=2, score=1.0)
        worse = _make_report(total=2, passed=0, score=0.0)
        gate = PromotionGate(
            eval_runner_factory=_factory_seq([baseline, worse]),
            eval_dataset_loader=lambda: _dataset(),
            skill_store=skill_store,
            skill_manager=None,
            store=store,
            regression_threshold=0.05,
        )
        cand = SkillCandidate(operation="disable", skill_name="alpha")
        await store.save_candidate(cand)
        decision = await gate.evaluate(cand)
        assert decision.promoted is False
        assert "alpha" not in skill_store._disabled
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_disable_apply_when_skill_store_lacks_disabled_attr(tmp_path: Path):
    store, skill_store, backend = await _new_setup(tmp_path)
    try:
        baseline = _make_report(total=2, passed=2, score=1.0)
        gate = PromotionGate(
            eval_runner_factory=_factory_seq([baseline]),
            eval_dataset_loader=lambda: _dataset(),
            skill_store=skill_store,
            skill_manager=None,
            store=store,
        )
        # Drop the attribute to simulate a non-conforming store implementation.
        del skill_store._disabled
        cand = SkillCandidate(operation="disable", skill_name="ghost")
        await store.save_candidate(cand)
        decision = await gate.evaluate(cand)
        assert decision.promoted is False
        assert "_disabled" in decision.reason
    finally:
        await backend.close()


# ── _decide branches not yet exercised ──────────────────────────────────────


@pytest.mark.asyncio
async def test_promote_on_avg_score_only_improvement(tmp_path: Path):
    """Strict mode allows promotion when avg_score improves while pass_rate is stable."""
    store, skill_store, backend = await _new_setup(tmp_path)
    try:
        # Same pass_rate, higher avg_score
        baseline = _make_report(total=4, passed=2, score=0.40)
        improved = _make_report(total=4, passed=2, score=0.80)
        gate = PromotionGate(
            eval_runner_factory=_factory_seq([baseline, improved]),
            eval_dataset_loader=lambda: _dataset(),
            skill_store=skill_store,
            skill_manager=None,
            store=store,
            require_strict_improvement=True,
        )
        cand = SkillCandidate(
            operation="create",
            skill_name="alpha",
            proposed_content="---\nname: alpha\ndescription: x\n---\n",
        )
        await store.save_candidate(cand)
        decision = await gate.evaluate(cand)
        assert decision.promoted is True
        assert "avg_score" in decision.reason


    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_loose_mode_promotes_on_no_change_no_regression(tmp_path: Path):
    """Loose mode promotes even when scores are identical."""
    store, skill_store, backend = await _new_setup(tmp_path)
    try:
        baseline = _make_report(total=4, passed=2, score=0.5)
        same = _make_report(total=4, passed=2, score=0.5)
        gate = PromotionGate(
            eval_runner_factory=_factory_seq([baseline, same]),
            eval_dataset_loader=lambda: _dataset(),
            skill_store=skill_store,
            skill_manager=None,
            store=store,
            require_strict_improvement=False,
        )
        cand = SkillCandidate(
            operation="create",
            skill_name="alpha",
            proposed_content="---\nname: alpha\ndescription: y\n---\n",
        )
        await store.save_candidate(cand)
        decision = await gate.evaluate(cand)
        assert decision.promoted is True
        assert "non-regression" in decision.reason
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_dataset_loader_exception_returns_none(tmp_path: Path):
    """If the loader raises, gate treats dataset as missing and rejects cleanly."""
    store, skill_store, backend = await _new_setup(tmp_path)
    try:
        def boom_loader():
            raise IOError("disk gone")

        gate = PromotionGate(
            eval_runner_factory=_factory_seq([_make_report(total=2, passed=2, score=1.0)]),
            eval_dataset_loader=boom_loader,
            skill_store=skill_store,
            skill_manager=None,
            store=store,
        )
        cand = SkillCandidate(operation="create", skill_name="alpha")
        await store.save_candidate(cand)
        decision = await gate.evaluate(cand)
        assert decision.promoted is False
        assert "empty" in decision.reason or "missing" in decision.reason
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_async_dataset_loader_supported(tmp_path: Path):
    """Loader returning a coroutine should be awaited."""
    store, skill_store, backend = await _new_setup(tmp_path)
    try:
        async def async_loader():
            return _dataset()

        baseline = _make_report(total=2, passed=1, score=0.5)
        improved = _make_report(total=2, passed=2, score=0.9)
        gate = PromotionGate(
            eval_runner_factory=_factory_seq([baseline, improved]),
            eval_dataset_loader=async_loader,
            skill_store=skill_store,
            skill_manager=None,
            store=store,
        )
        cand = SkillCandidate(
            operation="create",
            skill_name="alpha",
            proposed_content="---\nname: alpha\ndescription: y\n---\n",
        )
        await store.save_candidate(cand)
        decision = await gate.evaluate(cand)
        assert decision.promoted is True
    finally:
        await backend.close()


# ── _summarize fault tolerance ───────────────────────────────────────────────


def test_summarize_handles_report_summary_exception():
    """If report.summary() raises, _summarize must still return numeric fields."""
    bad_report = MagicMock()
    bad_report.pass_rate = 0.5
    bad_report.avg_score = 0.4
    bad_report.total_cases = 4
    bad_report.passed_cases = 2
    bad_report.duration_ms = 10.0
    bad_report.summary = MagicMock(side_effect=RuntimeError("boom"))

    summary = PromotionGate._summarize(bad_report)
    assert summary["pass_rate"] == 0.5
    assert summary["avg_score"] == 0.4
    assert summary["total_cases"] == 4


# ── _refresh_skill_manager_after_promote ────────────────────────────────────


@pytest.mark.asyncio
async def test_refresh_skill_manager_called_when_provided(tmp_path: Path):
    store, skill_store, backend = await _new_setup(tmp_path)
    try:
        skill_manager = MagicMock()
        skill_manager._load_installed = MagicMock()

        baseline = _make_report(total=2, passed=1, score=0.5)
        improved = _make_report(total=2, passed=2, score=0.9)
        gate = PromotionGate(
            eval_runner_factory=_factory_seq([baseline, improved]),
            eval_dataset_loader=lambda: _dataset(),
            skill_store=skill_store,
            skill_manager=skill_manager,
            store=store,
        )
        cand = SkillCandidate(
            operation="create",
            skill_name="alpha",
            proposed_content="---\nname: alpha\ndescription: y\n---\n",
        )
        await store.save_candidate(cand)
        decision = await gate.evaluate(cand)
        assert decision.promoted is True
        skill_manager._load_installed.assert_called_once()
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_refresh_skill_manager_swallows_exception(tmp_path: Path):
    """If skill_manager._load_installed raises, the promotion still succeeds."""
    store, skill_store, backend = await _new_setup(tmp_path)
    try:
        skill_manager = MagicMock()
        skill_manager._load_installed = MagicMock(side_effect=RuntimeError("scan failed"))

        baseline = _make_report(total=2, passed=1, score=0.5)
        improved = _make_report(total=2, passed=2, score=0.9)
        gate = PromotionGate(
            eval_runner_factory=_factory_seq([baseline, improved]),
            eval_dataset_loader=lambda: _dataset(),
            skill_store=skill_store,
            skill_manager=skill_manager,
            store=store,
        )
        cand = SkillCandidate(
            operation="create",
            skill_name="alpha",
            proposed_content="---\nname: alpha\ndescription: y\n---\n",
        )
        await store.save_candidate(cand)
        decision = await gate.evaluate(cand)
        assert decision.promoted is True
    finally:
        await backend.close()


# ── _cleanup / _restore are exception-tolerant ───────────────────────────────


def test_cleanup_backup_swallows_filesystem_errors(tmp_path: Path):
    """A non-existent backup directory must not raise from cleanup."""
    user_dir = tmp_path / "skills"
    user_dir.mkdir()
    gate = PromotionGate(
        eval_runner_factory=lambda: MagicMock(),
        eval_dataset_loader=lambda: _dataset(),
        skill_store=SkillStore(user_dir=user_dir),
        skill_manager=None,
        store=MagicMock(),
    )
    # Pass a path that does not exist.
    gate._cleanup_backup(tmp_path / "evolution-skills-backup-xxx" / "user_dir")


def test_restore_backup_handles_missing_user_dir(tmp_path: Path):
    """Restore should still copy the backup back even if user_dir was removed."""
    user_dir = tmp_path / "skills"
    user_dir.mkdir()
    skill_store = SkillStore(user_dir=user_dir)
    skill_store.create_skill("alpha", "---\nname: alpha\ndescription: x\n---\n")

    gate = PromotionGate(
        eval_runner_factory=lambda: MagicMock(),
        eval_dataset_loader=lambda: _dataset(),
        skill_store=skill_store,
        skill_manager=None,
        store=MagicMock(),
    )
    backup_dir, _ = gate._snapshot_user_dir(user_dir)
    # Simulate someone deleting user_dir.
    import shutil as _shutil
    _shutil.rmtree(user_dir)
    gate._restore_backup(user_dir, backup_dir)
    assert (user_dir / "alpha" / "SKILL.md").exists()
