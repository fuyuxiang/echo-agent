"""M2 evolution-gate hardening — end-to-end wiring regressions.

Each test pins a fail-closed behaviour that was specified in
docs/superpowers/specs/2026-06-16-m2-evolution-gate-hardening-design.md but
only landed at the data layer. They fail if the gate/engine stops consuming
the protective signal (protected-skill block, judge inconclusive,
safety-category regression, cooldown persistence).
"""

from __future__ import annotations

import json
import time
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


def _report(cases: list[CaseResult]) -> EvalReport:
    passed = sum(1 for c in cases if c.passed)
    return EvalReport(
        results=cases, total_cases=len(cases), passed_cases=passed, duration_ms=1.0
    )


def _case(case_id: str, *, passed: bool, category: str = "", inconclusive: bool = False) -> CaseResult:
    return CaseResult(
        case_id=case_id,
        passed=passed,
        category=category,
        metrics=[MetricResult(name="m", score=1.0 if passed else 0.0, passed=passed, inconclusive=inconclusive)],
    )


def _factory_seq(reports: list[EvalReport]):
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


def _dataset(case_count: int = 2):
    ds = MagicMock()
    ds.cases = list(range(case_count))
    return ds


async def _new_setup(tmp_path: Path, *, with_builtin: bool = False):
    backend = SQLiteBackend(tmp_path / "evolution.db")
    await backend.initialize()
    store = TrajectoryStore(backend)
    await store.init_schema()
    user_dir = tmp_path / "skills"
    user_dir.mkdir()
    builtin_dir = None
    if with_builtin:
        builtin_dir = tmp_path / "builtin"
        builtin_dir.mkdir()
        skill_dir = builtin_dir / "guardrail"
        skill_dir.mkdir()
        (skill_dir / "SKILL.md").write_text(
            "---\nname: guardrail\ndescription: builtin safety skill\n---\n",
            encoding="utf-8",
        )
    skill_store = SkillStore(user_dir=user_dir, builtin_dir=builtin_dir)
    return store, skill_store, backend


# ── 1. protected-skill block (disable bypass) ────────────────────────────────


@pytest.mark.asyncio
async def test_disable_protected_builtin_skill_is_rejected_before_candidate_eval(tmp_path: Path):
    store, skill_store, backend = await _new_setup(tmp_path, with_builtin=True)
    try:
        assert skill_store.is_protected("guardrail") is True
        # Sequence has only the baseline report; if the candidate eval ran it
        # would advance past index 0. We assert it never runs.
        baseline = _report([_case("c0", passed=True)])
        factory = _factory_seq([baseline])
        gate = PromotionGate(
            eval_runner_factory=factory,
            eval_dataset_loader=lambda: _dataset(),
            skill_store=skill_store,
            store=store,
        )
        cand = SkillCandidate(operation="disable", skill_name="guardrail")
        await store.save_candidate(cand)
        decision = await gate.evaluate(cand)
        assert decision.promoted is False
        assert "protected" in decision.reason.lower()
        # Candidate (with-candidate) eval was skipped — only baseline consumed.
        assert decision.with_candidate is None
        # The builtin skill must not have been added to the disabled set.
        assert "guardrail" not in getattr(skill_store, "_disabled", set())
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_patch_protected_builtin_skill_is_rejected(tmp_path: Path):
    store, skill_store, backend = await _new_setup(tmp_path, with_builtin=True)
    try:
        gate = PromotionGate(
            eval_runner_factory=_factory_seq([_report([_case("c0", passed=True)])]),
            eval_dataset_loader=lambda: _dataset(),
            skill_store=skill_store,
            store=store,
        )
        cand = SkillCandidate(
            operation="patch",
            skill_name="guardrail",
            proposed_patch_old="builtin safety skill",
            proposed_patch_new="weakened",
        )
        await store.save_candidate(cand)
        decision = await gate.evaluate(cand)
        assert decision.promoted is False
        assert "protected" in decision.reason.lower()
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_disable_user_skill_is_not_blocked_by_protection(tmp_path: Path):
    store, skill_store, backend = await _new_setup(tmp_path)
    try:
        skill_store.create_skill("mine", "---\nname: mine\ndescription: user skill\n---\n")
        assert skill_store.is_protected("mine") is False
        # baseline pass, candidate pass with improvement → promote path reachable
        baseline = _report([_case("c0", passed=False)])
        with_cand = _report([_case("c0", passed=True)])
        gate = PromotionGate(
            eval_runner_factory=_factory_seq([baseline, with_cand]),
            eval_dataset_loader=lambda: _dataset(),
            skill_store=skill_store,
            store=store,
        )
        cand = SkillCandidate(operation="disable", skill_name="mine")
        await store.save_candidate(cand)
        decision = await gate.evaluate(cand)
        # Not blocked by the protection gate (reason is a decision, not "protected").
        assert "protected" not in decision.reason.lower()
    finally:
        await backend.close()


# ── 2. judge inconclusive → fail-closed ──────────────────────────────────────


@pytest.mark.asyncio
async def test_candidate_inconclusive_blocks_promotion(tmp_path: Path):
    store, skill_store, backend = await _new_setup(tmp_path)
    try:
        # Baseline clean; candidate report has an inconclusive case even though
        # aggregate pass_rate would otherwise improve.
        baseline = _report([_case("c0", passed=True)])
        with_cand = _report([_case("c0", passed=True, inconclusive=True)])
        gate = PromotionGate(
            eval_runner_factory=_factory_seq([baseline, with_cand]),
            eval_dataset_loader=lambda: _dataset(),
            skill_store=skill_store,
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
        assert "inconclusive" in decision.reason.lower()
    finally:
        await backend.close()


# ── 3. safety-category zero-tolerance regression ─────────────────────────────


@pytest.mark.asyncio
async def test_safety_category_regression_rejects_despite_higher_pass_rate(tmp_path: Path):
    store, skill_store, backend = await _new_setup(tmp_path)
    try:
        # Baseline: safety passes, two tool cases fail (pass_rate 1/3).
        baseline = _report([
            _case("safe1", passed=True, category="safety"),
            _case("tool1", passed=False, category="tool"),
            _case("tool2", passed=False, category="tool"),
        ])
        # Candidate: safety REGRESSES but both tool cases now pass (pass_rate 2/3
        # — strictly higher). Must still be rejected on the safety regression.
        with_cand = _report([
            _case("safe1", passed=False, category="safety"),
            _case("tool1", passed=True, category="tool"),
            _case("tool2", passed=True, category="tool"),
        ])
        gate = PromotionGate(
            eval_runner_factory=_factory_seq([baseline, with_cand]),
            eval_dataset_loader=lambda: _dataset(),
            skill_store=skill_store,
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
        assert "safety regression" in decision.reason.lower()
    finally:
        await backend.close()


def test_regressed_categories_helper_matches_by_case_id():
    baseline = _report([
        _case("s1", passed=True, category="safety"),
        _case("r1", passed=True, category="refusal"),
        _case("t1", passed=False, category="tool"),
    ])
    with_cand = _report([
        _case("s1", passed=False, category="safety"),   # regressed
        _case("r1", passed=True, category="refusal"),    # stable
        _case("t1", passed=True, category="tool"),        # improved, not protected
    ])
    regressed = with_cand.regressed_categories(baseline, {"safety", "refusal"})
    assert regressed == {"safety"}


# ── 4. cooldown persistence ──────────────────────────────────────────────────


def test_cooldown_persists_across_engine_restart(tmp_path: Path, monkeypatch):
    """Activating a cooldown writes it to disk; a fresh engine sees it active."""
    from echo_agent.evolution import engine as engine_mod

    user_dir = tmp_path / "skills"
    user_dir.mkdir()

    # A minimal stand-in engine exercising only the cooldown mixin behaviour:
    # construct two real engines pointed at the same user_dir is heavyweight,
    # so we drive the persistence helpers directly on the real class via a
    # lightweight shim that shares the production methods.
    class _Shim:
        _save_cooldowns = engine_mod.EvolutionEngine._save_cooldowns
        _load_cooldowns = engine_mod.EvolutionEngine._load_cooldowns
        _activate_cooldown = engine_mod.EvolutionEngine._activate_cooldown
        _is_in_cooldown = engine_mod.EvolutionEngine._is_in_cooldown
        _cooldowns_path = engine_mod.EvolutionEngine._cooldowns_path

        def __init__(self, seconds: int):
            self._skill_store = SkillStore(user_dir=user_dir)
            self._config = MagicMock()
            self._config.cooldown_seconds_after_promote = seconds
            self._cooldowns = {}
            self._load_cooldowns()

    first = _Shim(seconds=3600)
    first._activate_cooldown("evolved-skill")
    assert first._is_in_cooldown("evolved-skill") is True

    cooldown_file = user_dir / ".evolution_cooldowns.json"
    assert cooldown_file.exists()
    payload = json.loads(cooldown_file.read_text(encoding="utf-8"))
    assert "evolved-skill" in payload

    # Simulate a restart: a brand-new instance loads from disk.
    second = _Shim(seconds=3600)
    assert second._is_in_cooldown("evolved-skill") is True


def test_expired_cooldown_dropped_on_load(tmp_path: Path):
    from echo_agent.evolution import engine as engine_mod

    user_dir = tmp_path / "skills"
    user_dir.mkdir()
    # Pre-seed an already-expired cooldown on disk.
    (user_dir / ".evolution_cooldowns.json").write_text(
        json.dumps({"stale": time.time() - 10}), encoding="utf-8"
    )

    class _Shim:
        _load_cooldowns = engine_mod.EvolutionEngine._load_cooldowns
        _cooldowns_path = engine_mod.EvolutionEngine._cooldowns_path

        def __init__(self):
            self._skill_store = SkillStore(user_dir=user_dir)
            self._cooldowns = {}
            self._load_cooldowns()

    shim = _Shim()
    assert "stale" not in shim._cooldowns
