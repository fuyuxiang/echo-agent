"""Tests for echo_agent.cli.evolution_cmd — CLI dispatcher and per-action flows.

These tests exercise the per-action async helpers (``_status``, ``_run_once``,
etc.) directly rather than the synchronous ``run_evolution_command`` wrapper,
which would call ``asyncio.run`` internally and leak event-loop state into
later tests in the same pytest session.
"""

from __future__ import annotations

import json
from pathlib import Path
from unittest.mock import AsyncMock, MagicMock, patch

import pytest
import yaml

from echo_agent.cli import evolution_cmd
from echo_agent.evolution.gate import PromotionDecision
from echo_agent.evolution.types import EvolutionRun, SkillCandidate


# ── Helpers ──────────────────────────────────────────────────────────────────


def _make_engine(
    *,
    summary: dict | None = None,
    runs: list[EvolutionRun] | None = None,
    candidates: list[SkillCandidate] | None = None,
    candidate_lookup: dict[str, SkillCandidate] | None = None,
    rollback_result: tuple[bool, str] = (True, "rolled back"),
    run_evolution_result: EvolutionRun | None = None,
    decision: PromotionDecision | None = None,
) -> MagicMock:
    """Construct a fully-async-mocked EvolutionEngine for CLI tests."""
    engine = MagicMock()
    engine.start = AsyncMock()
    engine.stop = AsyncMock()
    engine.status_summary = AsyncMock(return_value=summary or {
        "enabled": True,
        "trigger_mode": "manual",
        "scheduler_active": False,
        "trajectories_unconsumed": 0,
        "candidates_pending": 0,
        "candidates_needs_review": 0,
        "candidates_promoted_total": 0,
        "cooldowns": [],
    })
    engine.list_recent_runs = AsyncMock(return_value=runs or [])
    engine.list_candidates = AsyncMock(return_value=candidates or [])
    engine.rollback_skill = AsyncMock(return_value=rollback_result)
    engine.run_evolution = AsyncMock(return_value=run_evolution_result or EvolutionRun(triggered_by="manual"))

    store = MagicMock()
    store.get_candidate = AsyncMock(side_effect=lambda cid: (candidate_lookup or {}).get(cid))
    store.update_candidate = AsyncMock()
    engine.store = store

    gate = MagicMock()
    gate.evaluate = AsyncMock(return_value=decision or PromotionDecision(
        promoted=True, reason="ok",
        baseline={"pass_rate": 0.5}, with_candidate={"pass_rate": 0.9},
    ))
    engine._gate = gate
    return engine


def _make_ctx(engine: MagicMock | None) -> MagicMock:
    ctx = MagicMock()
    agent = MagicMock()
    agent.evolution = engine
    agent.start = AsyncMock()
    agent.stop = AsyncMock()
    bus = MagicMock()
    bus.start = AsyncMock()
    bus.stop = AsyncMock()
    storage = MagicMock()
    storage.close = AsyncMock()
    ctx.agent = agent
    ctx.bus = bus
    ctx.storage = storage
    return ctx


def _patch_bootstrap(ctx: MagicMock):
    """Patch the lazy import done inside each CLI helper."""
    return patch("echo_agent.__main__._bootstrap", AsyncMock(return_value=ctx))


# ── Dispatcher (synchronous validation paths only) ──────────────────────────
#
# These do not enter asyncio.run because the dispatcher exits before that.


def test_dispatcher_unknown_action_exits(capsys):
    with pytest.raises(SystemExit) as exc:
        evolution_cmd.run_evolution_command(action="not-a-real-action")
    assert exc.value.code == 1
    out = capsys.readouterr().out
    assert "Unknown evolution action" in out
    assert "Available:" in out


def test_dispatcher_show_candidate_requires_id(capsys):
    with pytest.raises(SystemExit) as exc:
        evolution_cmd.run_evolution_command(action="show-candidate")
    assert exc.value.code == 1
    assert "Usage:" in capsys.readouterr().out


def test_dispatcher_promote_requires_id(capsys):
    with pytest.raises(SystemExit) as exc:
        evolution_cmd.run_evolution_command(action="promote")
    assert exc.value.code == 1
    assert "Usage:" in capsys.readouterr().out


def test_dispatcher_rollback_requires_skill(capsys):
    with pytest.raises(SystemExit) as exc:
        evolution_cmd.run_evolution_command(action="rollback")
    assert exc.value.code == 1
    assert "Usage:" in capsys.readouterr().out


# ── _format_run ──────────────────────────────────────────────────────────────


def test_format_run_handles_none():
    assert "(no runs yet)" in evolution_cmd._format_run(None)


def test_format_run_renders_all_fields():
    run = EvolutionRun(
        id="run_xyz",
        triggered_by="threshold",
        trajectories_consumed=7,
        candidates_generated=3,
        candidates_promoted=2,
        candidates_rejected=1,
        candidates_needs_review=0,
        duration_ms=1234.5,
        finished_at="2026-05-22T10:00:00",
        error="",
    )
    rendered = evolution_cmd._format_run(run)
    assert "run_xyz" in rendered
    assert "threshold" in rendered
    assert "consumed     : 7" in rendered
    assert "1234.5" in rendered
    assert "2026-05-22T10:00:00" in rendered


def test_format_run_shows_in_progress_for_unfinished():
    run = EvolutionRun(triggered_by="manual", finished_at="")
    assert "(in progress)" in evolution_cmd._format_run(run)


# ── _status ──────────────────────────────────────────────────────────────────


@pytest.mark.asyncio
async def test_status_action_when_evolution_disabled(capsys):
    ctx = _make_ctx(engine=None)
    with _patch_bootstrap(ctx):
        await evolution_cmd._status(None, None)
    out = capsys.readouterr().out
    assert "Evolution is disabled" in out
    ctx.storage.close.assert_awaited_once()


@pytest.mark.asyncio
async def test_status_action_prints_summary_and_latest_run(capsys):
    run = EvolutionRun(id="run_abc", triggered_by="manual", candidates_promoted=1)
    engine = _make_engine(
        summary={
            "enabled": True,
            "trigger_mode": "threshold",
            "scheduler_active": True,
            "trajectories_unconsumed": 12,
            "candidates_pending": 2,
            "candidates_needs_review": 1,
            "candidates_promoted_total": 4,
            "cooldowns": [{"skill": "alpha", "until": "2026-05-22T10:00:00"}],
        },
        runs=[run],
    )
    ctx = _make_ctx(engine)
    with _patch_bootstrap(ctx):
        await evolution_cmd._status(None, None)
    out = capsys.readouterr().out
    assert "Evolution status" in out
    assert "trigger_mode         : threshold" in out
    assert "candidates pending      : 2" in out
    assert "alpha until 2026-05-22T10:00:00" in out
    assert "run_abc" in out
    engine.start.assert_awaited_once()
    engine.stop.assert_awaited_once()


@pytest.mark.asyncio
async def test_status_action_handles_no_runs(capsys):
    engine = _make_engine(runs=[])
    ctx = _make_ctx(engine)
    with _patch_bootstrap(ctx):
        await evolution_cmd._status(None, None)
    out = capsys.readouterr().out
    assert "(no runs yet)" in out


@pytest.mark.asyncio
async def test_status_action_closes_storage_on_engine_error():
    """If status_summary raises, ctx.storage.close must still run."""
    engine = _make_engine()
    engine.status_summary = AsyncMock(side_effect=RuntimeError("boom"))
    ctx = _make_ctx(engine)
    with _patch_bootstrap(ctx):
        with pytest.raises(RuntimeError):
            await evolution_cmd._status(None, None)
    ctx.storage.close.assert_awaited_once()
    engine.stop.assert_awaited_once()


# ── _run_once ────────────────────────────────────────────────────────────────


@pytest.mark.asyncio
async def test_run_action_when_evolution_disabled(capsys):
    ctx = _make_ctx(engine=None)
    with _patch_bootstrap(ctx):
        await evolution_cmd._run_once(None, None)
    assert "Evolution is disabled" in capsys.readouterr().out


@pytest.mark.asyncio
async def test_run_action_prints_run_summary(capsys):
    run = EvolutionRun(id="run_done", candidates_promoted=2, candidates_rejected=1)
    engine = _make_engine(run_evolution_result=run)
    ctx = _make_ctx(engine)
    with _patch_bootstrap(ctx):
        await evolution_cmd._run_once(None, None)
    out = capsys.readouterr().out
    assert "run_done" in out
    assert "Running evolution pass" in out
    engine.run_evolution.assert_awaited_once_with(trigger="manual")
    ctx.bus.start.assert_awaited_once()
    ctx.bus.stop.assert_awaited_once()
    ctx.agent.start.assert_awaited_once()
    ctx.agent.stop.assert_awaited_once()


# ── _list_candidates ─────────────────────────────────────────────────────────


@pytest.mark.asyncio
async def test_list_candidates_when_disabled(capsys):
    ctx = _make_ctx(engine=None)
    with _patch_bootstrap(ctx):
        await evolution_cmd._list_candidates(None, None, "")
    assert "Evolution is disabled" in capsys.readouterr().out


@pytest.mark.asyncio
async def test_list_candidates_empty(capsys):
    engine = _make_engine(candidates=[])
    ctx = _make_ctx(engine)
    with _patch_bootstrap(ctx):
        await evolution_cmd._list_candidates(None, None, "")
    assert "(no candidates)" in capsys.readouterr().out


@pytest.mark.asyncio
async def test_list_candidates_renders_rows(capsys):
    cands = [
        SkillCandidate(id="cand_1", operation="create", skill_name="alpha"),
        SkillCandidate(id="cand_2", operation="patch", skill_name="beta", status="promoted"),
    ]
    engine = _make_engine(candidates=cands)
    ctx = _make_ctx(engine)
    with _patch_bootstrap(ctx):
        await evolution_cmd._list_candidates(None, None, "")
    out = capsys.readouterr().out
    assert "cand_1" in out
    assert "cand_2" in out
    assert "alpha" in out
    assert "beta" in out
    assert "STATUS" in out


@pytest.mark.asyncio
async def test_list_candidates_passes_status_filter():
    engine = _make_engine(candidates=[])
    ctx = _make_ctx(engine)
    with _patch_bootstrap(ctx):
        await evolution_cmd._list_candidates(None, None, "needs_review")
    engine.list_candidates.assert_awaited_with(status="needs_review", limit=200)


@pytest.mark.asyncio
async def test_list_candidates_empty_filter_passes_none_to_engine():
    engine = _make_engine(candidates=[])
    ctx = _make_ctx(engine)
    with _patch_bootstrap(ctx):
        await evolution_cmd._list_candidates(None, None, "")
    engine.list_candidates.assert_awaited_with(status=None, limit=200)


# ── _show_candidate ──────────────────────────────────────────────────────────


@pytest.mark.asyncio
async def test_show_candidate_when_disabled(capsys):
    ctx = _make_ctx(engine=None)
    with _patch_bootstrap(ctx):
        await evolution_cmd._show_candidate("cand_x", None, None)
    assert "Evolution is disabled" in capsys.readouterr().out


@pytest.mark.asyncio
async def test_show_candidate_not_found(capsys):
    engine = _make_engine(candidate_lookup={})
    ctx = _make_ctx(engine)
    with _patch_bootstrap(ctx):
        with pytest.raises(SystemExit) as exc:
            await evolution_cmd._show_candidate("missing", None, None)
    assert exc.value.code == 1
    assert "not found" in capsys.readouterr().out


@pytest.mark.asyncio
async def test_show_candidate_prints_full_json(capsys):
    cand = SkillCandidate(
        id="cand_show", operation="create", skill_name="alpha",
        rationale="r", expected_improvement="i",
    )
    engine = _make_engine(candidate_lookup={"cand_show": cand})
    ctx = _make_ctx(engine)
    with _patch_bootstrap(ctx):
        await evolution_cmd._show_candidate("cand_show", None, None)
    out = capsys.readouterr().out
    payload = json.loads(out)
    assert payload["id"] == "cand_show"
    assert payload["skill_name"] == "alpha"


# ── _promote_candidate ───────────────────────────────────────────────────────


@pytest.mark.asyncio
async def test_promote_when_disabled(capsys):
    ctx = _make_ctx(engine=None)
    with _patch_bootstrap(ctx):
        await evolution_cmd._promote_candidate("cand_x", None, None)
    assert "Evolution is disabled" in capsys.readouterr().out


@pytest.mark.asyncio
async def test_promote_unknown_candidate_exits(capsys):
    engine = _make_engine(candidate_lookup={})
    ctx = _make_ctx(engine)
    with _patch_bootstrap(ctx):
        with pytest.raises(SystemExit) as exc:
            await evolution_cmd._promote_candidate("missing", None, None)
    assert exc.value.code == 1
    assert "not found" in capsys.readouterr().out


@pytest.mark.asyncio
async def test_promote_rejects_candidate_in_terminal_status(capsys):
    cand = SkillCandidate(id="cand_already", status="promoted", skill_name="alpha")
    engine = _make_engine(candidate_lookup={"cand_already": cand})
    ctx = _make_ctx(engine)
    with _patch_bootstrap(ctx):
        with pytest.raises(SystemExit) as exc:
            await evolution_cmd._promote_candidate("cand_already", None, None)
    assert exc.value.code == 1
    out = capsys.readouterr().out
    assert "promoted" in out
    assert "only pending" in out


@pytest.mark.asyncio
async def test_promote_runs_gate_and_prints_decision(capsys):
    cand = SkillCandidate(
        id="cand_p", status="needs_review", skill_name="alpha",
        rejected_reason="held for review",
    )
    decision = PromotionDecision(
        promoted=True, reason="great",
        baseline={"pass_rate": 0.5}, with_candidate={"pass_rate": 0.9},
    )
    engine = _make_engine(candidate_lookup={"cand_p": cand}, decision=decision)
    ctx = _make_ctx(engine)
    with _patch_bootstrap(ctx):
        await evolution_cmd._promote_candidate("cand_p", None, None)
    out = capsys.readouterr().out
    payload = json.loads(out)
    assert payload["promoted"] is True
    assert payload["reason"] == "great"
    assert payload["with_candidate"]["pass_rate"] == 0.9
    assert cand.status == "pending"
    assert cand.rejected_reason == ""
    engine.store.update_candidate.assert_awaited()
    engine._gate.evaluate.assert_awaited_once()


# ── _rollback ────────────────────────────────────────────────────────────────


@pytest.mark.asyncio
async def test_rollback_when_disabled(capsys):
    ctx = _make_ctx(engine=None)
    with _patch_bootstrap(ctx):
        await evolution_cmd._rollback("alpha", None, None)
    assert "Evolution is disabled" in capsys.readouterr().out


@pytest.mark.asyncio
async def test_rollback_success(capsys):
    engine = _make_engine(rollback_result=(True, "skill 'alpha' rolled back"))
    ctx = _make_ctx(engine)
    with _patch_bootstrap(ctx):
        await evolution_cmd._rollback("alpha", None, None)
    out = capsys.readouterr().out
    assert "rolled back" in out
    engine.rollback_skill.assert_awaited_once_with("alpha")


@pytest.mark.asyncio
async def test_rollback_failure_exits(capsys):
    engine = _make_engine(rollback_result=(False, "no promoted candidate"))
    ctx = _make_ctx(engine)
    with _patch_bootstrap(ctx):
        with pytest.raises(SystemExit) as exc:
            await evolution_cmd._rollback("missing", None, None)
    assert exc.value.code == 1
    assert "no promoted candidate" in capsys.readouterr().out


# ── _init_dataset (synchronous helper, no event loop involved) ─────────────


def test_init_dataset_writes_file(tmp_path: Path, capsys):
    evolution_cmd._init_dataset(None, str(tmp_path))
    target = tmp_path / "data" / "eval" / "baseline.yaml"
    assert target.exists()
    parsed = yaml.safe_load(target.read_text(encoding="utf-8"))
    assert "cases" in parsed
    assert any(c["id"] == "chat_smoke" for c in parsed["cases"])
    assert "Wrote baseline dataset" in capsys.readouterr().out


def test_init_dataset_does_not_overwrite_existing(tmp_path: Path, capsys):
    target = tmp_path / "data" / "eval" / "baseline.yaml"
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text("custom: marker\n", encoding="utf-8")

    evolution_cmd._init_dataset(None, str(tmp_path))

    assert target.read_text(encoding="utf-8") == "custom: marker\n"
    assert "already exists" in capsys.readouterr().out


def test_init_dataset_creates_parent_directories(tmp_path: Path, capsys):
    """Even if data/eval does not yet exist, init-dataset must create it."""
    workspace = tmp_path / "fresh"
    workspace.mkdir()
    assert not (workspace / "data").exists()
    evolution_cmd._init_dataset(None, str(workspace))
    assert (workspace / "data" / "eval" / "baseline.yaml").exists()


# ── Dispatcher integration with init-dataset (still synchronous) ────────────


def test_dispatcher_routes_init_dataset(tmp_path: Path):
    """init-dataset is the only action that does NOT call asyncio.run, so it
    is safe to exercise via run_evolution_command itself."""
    evolution_cmd.run_evolution_command(
        action="init-dataset", workspace=str(tmp_path),
    )
    assert (tmp_path / "data" / "eval" / "baseline.yaml").exists()
