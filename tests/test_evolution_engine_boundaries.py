"""Boundary tests for EvolutionEngine.

Covers code paths that the integration suite (test_evolution_engine.py) does
not exercise: cooldowns, auto_promote off, gate exceptions, rollback variants,
status_summary, retention purge, lifecycle re-entry, and helper internals.
"""

from __future__ import annotations

import time
from pathlib import Path
from unittest.mock import AsyncMock, MagicMock

import pytest

from echo_agent.config.schema import EvolutionConfig
from echo_agent.evolution.engine import EvolutionEngine, _Cooldown
from echo_agent.evolution.types import (
    EvolutionRun,
    SkillCandidate,
    ToolCall,
    Trajectory,
)
from echo_agent.plugins.hooks import HookRegistry
from echo_agent.skills.store import SkillStore
from echo_agent.storage.sqlite import SQLiteBackend


def _llm_response(content="", tool_calls=None):
    r = MagicMock()
    r.content = content
    r.tool_calls = tool_calls or []
    r.has_tool_calls = bool(tool_calls)
    r.finish_reason = "stop"
    return r


def _propose_call(args, tc_id="tc1"):
    tc = MagicMock()
    tc.id = tc_id
    tc.name = "propose_skill"
    tc.arguments = args
    tc.to_openai_format.return_value = {
        "id": tc_id, "type": "function",
        "function": {"name": "propose_skill", "arguments": args},
    }
    return tc


def _make_traj(outcome="failure", reflection_score=None) -> Trajectory:
    return Trajectory(
        session_id="s",
        chat_id="c",
        channel="cli",
        task_input="task",
        task_type="chat",
        outcome=outcome,
        iterations=2,
        tools_called=[ToolCall(name="x", success=False)],
        failure_reason="err" if outcome == "failure" else "",
        reflection_score=reflection_score,
    )


async def _new_engine(
    tmp_path: Path,
    *,
    config: EvolutionConfig,
    hooks=None,
    eval_factory=None,
    dataset_loader=None,
    provider=None,
):
    backend = SQLiteBackend(tmp_path / "db.sqlite")
    await backend.initialize()
    skill_store = SkillStore(user_dir=tmp_path / "skills")
    engine = EvolutionEngine(
        config=config,
        workspace=tmp_path,
        storage=backend,
        provider=provider or AsyncMock(),
        skill_store=skill_store,
        skill_manager=None,
        eval_runner_factory=eval_factory or (lambda: MagicMock()),
        eval_dataset_loader=dataset_loader or (lambda: MagicMock(cases=[1])),
        hooks=hooks,
        reflection=None,
    )
    return engine, backend, skill_store


# ── Lifecycle ────────────────────────────────────────────────────────────────


@pytest.mark.asyncio
async def test_start_is_idempotent(tmp_path: Path):
    config = EvolutionConfig(enabled=True, record_trajectories=False)
    engine, backend, _ = await _new_engine(tmp_path, config=config)
    try:
        await engine.start()
        await engine.start()  # second start must be a no-op
        assert engine._started is True
    finally:
        await engine.stop()
        await backend.close()


@pytest.mark.asyncio
async def test_stop_before_start_is_safe(tmp_path: Path):
    config = EvolutionConfig(enabled=True, record_trajectories=False)
    engine, backend, _ = await _new_engine(tmp_path, config=config)
    try:
        await engine.stop()  # never started
        assert engine._started is False
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_start_attaches_recorder_hooks_when_enabled(tmp_path: Path):
    config = EvolutionConfig(enabled=True, record_trajectories=True)
    hooks = HookRegistry()
    engine, backend, _ = await _new_engine(tmp_path, config=config, hooks=hooks)
    try:
        await engine.start()
        registered = hooks.get_registered_hooks()
        assert "post_tool_call" in registered
        assert "evolution" in registered["post_tool_call"]
    finally:
        await engine.stop()
        await backend.close()


@pytest.mark.asyncio
async def test_stop_detaches_hooks(tmp_path: Path):
    config = EvolutionConfig(enabled=True, record_trajectories=True)
    hooks = HookRegistry()
    engine, backend, _ = await _new_engine(tmp_path, config=config, hooks=hooks)
    await engine.start()
    await engine.stop()
    try:
        registered = hooks.get_registered_hooks()
        assert "post_tool_call" not in registered or \
               "evolution" not in registered.get("post_tool_call", [])
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_start_runs_retention_purge(tmp_path: Path):
    config = EvolutionConfig(enabled=True, record_trajectories=False, trajectory_retention_days=1)
    engine, backend, _ = await _new_engine(tmp_path, config=config)
    try:
        await engine.store.init_schema()
        old = _make_traj()
        old.created_at = "1990-01-01T00:00:00"
        await engine.store.append_trajectory(old)
        await engine.start()
        # The purge runs on start.
        assert await engine.store.count_unconsumed() == 0
    finally:
        await engine.stop()
        await backend.close()


@pytest.mark.asyncio
async def test_start_tolerates_purge_exception(tmp_path: Path):
    config = EvolutionConfig(enabled=True, record_trajectories=False, trajectory_retention_days=1)
    engine, backend, _ = await _new_engine(tmp_path, config=config)
    try:
        engine._store.purge_older_than = AsyncMock(side_effect=RuntimeError("io error"))
        await engine.start()
        assert engine._started is True
    finally:
        await engine.stop()
        await backend.close()


# ── Public API: list / status_summary ────────────────────────────────────────


@pytest.mark.asyncio
async def test_list_recent_runs_passes_through(tmp_path: Path):
    config = EvolutionConfig(enabled=True, record_trajectories=False)
    engine, backend, _ = await _new_engine(tmp_path, config=config)
    try:
        await engine.start()
        run_a = EvolutionRun(triggered_by="manual")
        run_b = EvolutionRun(triggered_by="threshold")
        await engine.store.save_run(run_a)
        await engine.store.save_run(run_b)
        runs = await engine.list_recent_runs(limit=5)
        ids = {r.id for r in runs}
        assert {run_a.id, run_b.id} <= ids
    finally:
        await engine.stop()
        await backend.close()


@pytest.mark.asyncio
async def test_list_candidates_passes_status_filter(tmp_path: Path):
    config = EvolutionConfig(enabled=True, record_trajectories=False)
    engine, backend, _ = await _new_engine(tmp_path, config=config)
    try:
        await engine.start()
        c_pending = SkillCandidate(operation="create", skill_name="alpha")
        c_promoted = SkillCandidate(operation="patch", skill_name="beta", status="promoted")
        await engine.store.save_candidate(c_pending)
        await engine.store.save_candidate(c_promoted)
        promoted = await engine.list_candidates(status="promoted")
        assert [c.id for c in promoted] == [c_promoted.id]
    finally:
        await engine.stop()
        await backend.close()


@pytest.mark.asyncio
async def test_status_summary_shape(tmp_path: Path):
    config = EvolutionConfig(enabled=True, record_trajectories=False, trigger_mode="threshold")
    engine, backend, _ = await _new_engine(tmp_path, config=config)
    try:
        await engine.start()
        engine._cooldowns["sk1"] = _Cooldown(skill_name="sk1", until_ts=time.time() + 60)
        await engine.store.save_run(EvolutionRun(triggered_by="manual"))
        summary = await engine.status_summary()
        assert summary["enabled"] is True
        assert summary["trigger_mode"] == "threshold"
        assert summary["scheduler_active"] is True
        assert summary["latest_run"] is not None
        assert any(cd["skill"] == "sk1" for cd in summary["cooldowns"])
    finally:
        await engine.stop()
        await backend.close()


@pytest.mark.asyncio
async def test_status_summary_with_no_runs(tmp_path: Path):
    config = EvolutionConfig(enabled=True, record_trajectories=False)
    engine, backend, _ = await _new_engine(tmp_path, config=config)
    try:
        await engine.start()
        summary = await engine.status_summary()
        assert summary["latest_run"] is None
        assert summary["cooldowns"] == []
    finally:
        await engine.stop()
        await backend.close()


# ── Run flow: cooldowns ──────────────────────────────────────────────────────


@pytest.mark.asyncio
async def test_run_skips_candidate_in_cooldown(tmp_path: Path):
    config = EvolutionConfig(
        enabled=True, record_trajectories=False, cooldown_seconds_after_promote=300,
    )
    provider = AsyncMock()
    provider.chat_with_retry = AsyncMock(side_effect=[
        _llm_response(tool_calls=[_propose_call({
            "operation": "create",
            "skill_name": "in-cooldown",
            "content": "---\nname: in-cooldown\ndescription: x\n---\n",
            "rationale": "r",
            "expected_improvement": "i",
        })]),
        _llm_response(content="done"),
    ])
    engine, backend, skill_store = await _new_engine(tmp_path, config=config, provider=provider)
    try:
        await engine.start()
        engine._activate_cooldown("in-cooldown")
        await engine.store.append_trajectory(_make_traj())
        run = await engine.run_evolution(trigger="manual")
        assert run.candidates_generated == 1
        assert run.candidates_rejected == 1
        assert run.candidates_promoted == 0
        # Candidate marked rejected for cooldown reason.
        cands = await engine.store.list_candidates(status="rejected")
        assert any("cooldown" in c.rejected_reason for c in cands)
        # Skill must not have been written.
        assert not (skill_store.user_dir / "in-cooldown" / "SKILL.md").exists()
    finally:
        await engine.stop()
        await backend.close()


def test_activate_cooldown_zero_seconds_is_noop(tmp_path: Path):
    """cooldown_seconds_after_promote=0 means cooldowns are disabled."""
    config = EvolutionConfig(enabled=True, cooldown_seconds_after_promote=0)
    engine = EvolutionEngine(
        config=config,
        workspace=tmp_path,
        storage=MagicMock(),
        provider=AsyncMock(),
        skill_store=MagicMock(),
        skill_manager=None,
        eval_runner_factory=lambda: MagicMock(),
        eval_dataset_loader=lambda: MagicMock(),
    )
    engine._activate_cooldown("alpha")
    assert "alpha" not in engine._cooldowns


def test_engine_passes_actual_config_to_gate_validation(tmp_path: Path):
    config = EvolutionConfig(
        enabled=True,
        auto_promote=False,
        require_strict_improvement=False,
        regression_threshold=0.2,
        cooldown_seconds_after_promote=0,
        max_candidates_per_run=12,
    )
    engine = EvolutionEngine(
        config=config,
        workspace=tmp_path,
        storage=MagicMock(),
        provider=AsyncMock(),
        skill_store=MagicMock(),
        skill_manager=None,
        eval_runner_factory=lambda: MagicMock(),
        eval_dataset_loader=lambda: MagicMock(),
    )

    assert engine._config is config


def test_activate_cooldown_empty_skill_name_is_noop(tmp_path: Path):
    config = EvolutionConfig(enabled=True, cooldown_seconds_after_promote=60)
    engine = EvolutionEngine(
        config=config, workspace=tmp_path, storage=MagicMock(),
        provider=AsyncMock(), skill_store=MagicMock(), skill_manager=None,
        eval_runner_factory=lambda: MagicMock(), eval_dataset_loader=lambda: MagicMock(),
    )
    engine._activate_cooldown("")
    assert engine._cooldowns == {}


def test_is_in_cooldown_expires(tmp_path: Path):
    config = EvolutionConfig(enabled=True, cooldown_seconds_after_promote=60)
    engine = EvolutionEngine(
        config=config, workspace=tmp_path, storage=MagicMock(),
        provider=AsyncMock(), skill_store=MagicMock(), skill_manager=None,
        eval_runner_factory=lambda: MagicMock(), eval_dataset_loader=lambda: MagicMock(),
    )
    engine._cooldowns["alpha"] = _Cooldown(skill_name="alpha", until_ts=time.time() - 1)
    assert engine._is_in_cooldown("alpha") is False
    # Expired entry should have been pruned.
    assert "alpha" not in engine._cooldowns


def test_is_in_cooldown_active(tmp_path: Path):
    config = EvolutionConfig(enabled=True, cooldown_seconds_after_promote=60)
    engine = EvolutionEngine(
        config=config, workspace=tmp_path, storage=MagicMock(),
        provider=AsyncMock(), skill_store=MagicMock(), skill_manager=None,
        eval_runner_factory=lambda: MagicMock(), eval_dataset_loader=lambda: MagicMock(),
    )
    engine._cooldowns["alpha"] = _Cooldown(skill_name="alpha", until_ts=time.time() + 60)
    assert engine._is_in_cooldown("alpha") is True


# ── Run flow: auto_promote=False ─────────────────────────────────────────────


@pytest.mark.asyncio
async def test_run_holds_candidates_when_auto_promote_disabled(tmp_path: Path):
    config = EvolutionConfig(
        enabled=True, record_trajectories=False, auto_promote=False,
    )
    provider = AsyncMock()
    provider.chat_with_retry = AsyncMock(side_effect=[
        _llm_response(tool_calls=[_propose_call({
            "operation": "create",
            "skill_name": "needs-review-skill",
            "content": "---\nname: needs-review-skill\ndescription: x\n---\n",
            "rationale": "r",
            "expected_improvement": "i",
        })]),
        _llm_response(content="done"),
    ])

    eval_called = MagicMock()

    def eval_factory():
        eval_called()
        return MagicMock()

    engine, backend, skill_store = await _new_engine(
        tmp_path, config=config, provider=provider, eval_factory=eval_factory,
    )
    try:
        await engine.start()
        await engine.store.append_trajectory(_make_traj())
        run = await engine.run_evolution(trigger="manual")
        assert run.candidates_needs_review == 1
        assert run.candidates_promoted == 0
        # Eval gate must not have been invoked.
        eval_called.assert_not_called()
        # Skill must not have been written.
        assert not (skill_store.user_dir / "needs-review-skill" / "SKILL.md").exists()
    finally:
        await engine.stop()
        await backend.close()


# ── Run flow: gate raises ────────────────────────────────────────────────────


@pytest.mark.asyncio
async def test_run_records_rejection_when_gate_raises(tmp_path: Path):
    config = EvolutionConfig(enabled=True, record_trajectories=False)
    provider = AsyncMock()
    provider.chat_with_retry = AsyncMock(side_effect=[
        _llm_response(tool_calls=[_propose_call({
            "operation": "create",
            "skill_name": "gate-blowup",
            "content": "---\nname: gate-blowup\ndescription: x\n---\n",
            "rationale": "r",
            "expected_improvement": "i",
        })]),
        _llm_response(content="done"),
    ])
    engine, backend, _ = await _new_engine(tmp_path, config=config, provider=provider)
    try:
        await engine.start()
        engine._gate.evaluate = AsyncMock(side_effect=RuntimeError("gate exploded"))
        await engine.store.append_trajectory(_make_traj())
        run = await engine.run_evolution(trigger="manual")
        assert run.candidates_rejected == 1
        cands = await engine.store.list_candidates(status="rejected")
        assert any("gate exploded" in c.rejected_reason for c in cands)
    finally:
        await engine.stop()
        await backend.close()


# ── Rollback ─────────────────────────────────────────────────────────────────


@pytest.mark.asyncio
async def test_rollback_unknown_skill(tmp_path: Path):
    config = EvolutionConfig(enabled=True, record_trajectories=False)
    engine, backend, _ = await _new_engine(tmp_path, config=config)
    try:
        await engine.start()
        ok, msg = await engine.rollback_skill("never-promoted")
        assert ok is False
        assert "no promoted candidate" in msg
    finally:
        await engine.stop()
        await backend.close()


@pytest.mark.asyncio
async def test_rollback_patch_inverts_change(tmp_path: Path):
    config = EvolutionConfig(enabled=True, record_trajectories=False)
    engine, backend, skill_store = await _new_engine(tmp_path, config=config)
    try:
        await engine.start()
        # Pre-create a skill.
        original = "---\nname: alpha\ndescription: original\n---\n"
        skill_store.create_skill("alpha", original)
        # Apply a patch and persist a "promoted" record for it.
        skill_store.patch_skill("alpha", "original", "patched")
        cand = SkillCandidate(
            operation="patch", skill_name="alpha", parent_skill="alpha",
            proposed_patch_old="original", proposed_patch_new="patched",
            status="promoted",
        )
        await engine.store.save_candidate(cand)

        ok, msg = await engine.rollback_skill("alpha")
        assert ok is True, msg
        body = (skill_store.user_dir / "alpha" / "SKILL.md").read_text(encoding="utf-8")
        assert "original" in body
        assert "patched" not in body

        refreshed = await engine.store.get_candidate(cand.id)
        assert refreshed.status == "rolled_back"
    finally:
        await engine.stop()
        await backend.close()


@pytest.mark.asyncio
async def test_rollback_patch_with_missing_payload_fails(tmp_path: Path):
    config = EvolutionConfig(enabled=True, record_trajectories=False)
    engine, backend, _ = await _new_engine(tmp_path, config=config)
    try:
        await engine.start()
        # No new text recorded — cannot invert.
        cand = SkillCandidate(
            operation="patch", skill_name="alpha",
            proposed_patch_old="original", proposed_patch_new="",
            status="promoted",
        )
        await engine.store.save_candidate(cand)
        ok, msg = await engine.rollback_skill("alpha")
        assert ok is False
        assert "no patch payload" in msg
    finally:
        await engine.stop()
        await backend.close()


@pytest.mark.asyncio
async def test_rollback_patch_when_text_missing_returns_error(tmp_path: Path):
    """If the skill no longer contains the new text (someone else edited it),
    the inverse patch should report a clean failure."""
    config = EvolutionConfig(enabled=True, record_trajectories=False)
    engine, backend, skill_store = await _new_engine(tmp_path, config=config)
    try:
        await engine.start()
        skill_store.create_skill("alpha", "---\nname: alpha\ndescription: original\n---\n")
        cand = SkillCandidate(
            operation="patch", skill_name="alpha",
            proposed_patch_old="original",
            proposed_patch_new="patched-but-now-missing",
            status="promoted",
        )
        await engine.store.save_candidate(cand)
        ok, msg = await engine.rollback_skill("alpha")
        assert ok is False
        assert "inverse patch failed" in msg
    finally:
        await engine.stop()
        await backend.close()


@pytest.mark.asyncio
async def test_rollback_disable_clears_disabled_set(tmp_path: Path):
    config = EvolutionConfig(enabled=True, record_trajectories=False)
    engine, backend, skill_store = await _new_engine(tmp_path, config=config)
    try:
        await engine.start()
        skill_store._disabled.add("alpha")
        cand = SkillCandidate(operation="disable", skill_name="alpha", status="promoted")
        await engine.store.save_candidate(cand)
        ok, msg = await engine.rollback_skill("alpha")
        assert ok is True
        assert "alpha" not in skill_store._disabled
    finally:
        await engine.stop()
        await backend.close()


@pytest.mark.asyncio
async def test_rollback_unknown_operation_fails(tmp_path: Path):
    config = EvolutionConfig(enabled=True, record_trajectories=False)
    engine, backend, _ = await _new_engine(tmp_path, config=config)
    try:
        await engine.start()
        cand = SkillCandidate(skill_name="alpha", status="promoted")
        cand.operation = "weird"  # type: ignore[assignment]
        await engine.store.save_candidate(cand)
        ok, msg = await engine.rollback_skill("alpha")
        assert ok is False
        assert "unknown operation" in msg
    finally:
        await engine.stop()
        await backend.close()


@pytest.mark.asyncio
async def test_rollback_swallows_underlying_exception(tmp_path: Path):
    config = EvolutionConfig(enabled=True, record_trajectories=False)
    engine, backend, skill_store = await _new_engine(tmp_path, config=config)
    try:
        await engine.start()
        cand = SkillCandidate(operation="create", skill_name="alpha", status="promoted")
        await engine.store.save_candidate(cand)
        skill_store.delete_skill = MagicMock(side_effect=RuntimeError("disk full"))
        ok, msg = await engine.rollback_skill("alpha")
        assert ok is False
        assert "rollback raised" in msg
    finally:
        await engine.stop()
        await backend.close()


# ── Run-level error path ───────────────────────────────────────────────────


@pytest.mark.asyncio
async def test_run_records_top_level_exception(tmp_path: Path):
    config = EvolutionConfig(enabled=True, record_trajectories=False)
    engine, backend, _ = await _new_engine(tmp_path, config=config)
    try:
        await engine.start()
        # Force collect_trajectories to explode.
        engine._collect_trajectories = AsyncMock(side_effect=RuntimeError("collect failed"))
        run = await engine.run_evolution(trigger="manual")
        assert "RuntimeError: collect failed" in run.error
        # finalize_run must still have been called (run persisted with finished_at).
        assert run.finished_at != ""
    finally:
        await engine.stop()
        await backend.close()


# ── Trajectory prioritisation ────────────────────────────────────────────────


@pytest.mark.asyncio
async def test_collect_trajectories_orders_failures_first(tmp_path: Path):
    config = EvolutionConfig(
        enabled=True, record_trajectories=False, max_trajectories_per_run=10,
    )
    engine, backend, _ = await _new_engine(tmp_path, config=config)
    try:
        await engine.start()
        ok = _make_traj(outcome="success", reflection_score=1.0)
        bad = _make_traj(outcome="failure")
        partial = _make_traj(outcome="partial", reflection_score=0.4)
        await engine.store.append_trajectory(ok)
        await engine.store.append_trajectory(bad)
        await engine.store.append_trajectory(partial)

        ordered = await engine._collect_trajectories()
        outcomes = [t.outcome for t in ordered]
        assert outcomes[0] == "failure"
        assert outcomes[1] == "partial"
        assert outcomes[2] == "success"
    finally:
        await engine.stop()
        await backend.close()


@pytest.mark.asyncio
async def test_collect_trajectories_caps_at_max(tmp_path: Path):
    config = EvolutionConfig(
        enabled=True, record_trajectories=False, max_trajectories_per_run=2,
    )
    engine, backend, _ = await _new_engine(tmp_path, config=config)
    try:
        await engine.start()
        for _ in range(5):
            await engine.store.append_trajectory(_make_traj(outcome="failure"))
        ordered = await engine._collect_trajectories()
        assert len(ordered) == 2
    finally:
        await engine.stop()
        await backend.close()


# ── Properties ───────────────────────────────────────────────────────────────


@pytest.mark.asyncio
async def test_store_and_recorder_properties_expose_internals(tmp_path: Path):
    config = EvolutionConfig(enabled=True, record_trajectories=False)
    engine, backend, _ = await _new_engine(tmp_path, config=config)
    try:
        assert engine.store is engine._store
        assert engine.recorder is engine._recorder
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_all_fetched_trajectories_marked_consumed(tmp_path: Path):
    """All trajectories fetched in a run should be marked consumed, even if the
    evolver doesn't reference them in its proposal."""
    config = EvolutionConfig(
        enabled=True, record_trajectories=False, max_trajectories_per_run=5,
        auto_promote=False,
    )
    engine, backend, _ = await _new_engine(tmp_path, config=config)
    try:
        await engine.start()
        for _ in range(3):
            await engine.store.append_trajectory(_make_traj(outcome="success", reflection_score=0.9))

        unconsumed_before = await engine.store.count_unconsumed()
        assert unconsumed_before == 3

        run = await engine.run_evolution(trigger="manual")

        unconsumed_after = await engine.store.count_unconsumed()
        assert unconsumed_after == 0
        assert run.trajectories_consumed == 3
    finally:
        await engine.stop()
        await backend.close()
