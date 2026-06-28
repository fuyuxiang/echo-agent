import pytest

from echo_agent.evolution.store import TrajectoryStore
from echo_agent.evolution.types import SkillCandidate
from echo_agent.skills.admission import SkillAdmission
from echo_agent.skills.store import SkillStore
from echo_agent.storage.sqlite import SQLiteBackend

@pytest.mark.asyncio
async def test_list_staged_and_approve_via_admission(tmp_path, capsys):
    backend = SQLiteBackend(tmp_path / "s.db")
    await backend.initialize()
    cstore = TrajectoryStore(backend)
    await cstore.init_schema()
    sstore = SkillStore(user_dir=tmp_path / "skills")
    adm = SkillAdmission(skill_store=sstore, candidate_store=cstore,
                         policy="stage_for_review", auto_write_risk="low")
    # 暂存一个高风险 create
    await adm.admit(SkillCandidate(
        operation="create", skill_name="newskill", source="reviewer", risk="high",
        proposed_content="---\nname: newskill\ndescription: d\n---\nbody",
    ))
    staged = await adm.list_staged()
    assert len(staged) == 1
    approved = await adm.approve(staged[0].id)
    assert approved.outcome == "written"
    assert sstore.read_skill("newskill") is not None
    await backend.close()


# ---------------------------------------------------------------------------
# CLI wrapper layer: echo_agent.cli.skill_admission_cmd.run_skill_command
# Covers action routing + sys.exit branches with _build_admission mocked out,
# so no bootstrap/storage is needed.
# ---------------------------------------------------------------------------

from types import SimpleNamespace
from unittest.mock import AsyncMock, patch

from echo_agent.cli import skill_admission_cmd


def _fake_admission():
    """Return (ctx, adm) doubles matching _build_admission's contract."""
    ctx = SimpleNamespace(storage=SimpleNamespace(close=AsyncMock()))
    adm = SimpleNamespace(
        list_staged=AsyncMock(return_value=[]),
        approve=AsyncMock(return_value=SimpleNamespace(outcome="written", message="ok")),
        reject=AsyncMock(return_value=SimpleNamespace(outcome="rejected", message="no")),
    )
    return ctx, adm


def test_run_skill_command_list_staged_empty(capsys):
    ctx, adm = _fake_admission()
    with patch.object(skill_admission_cmd, "_build_admission",
                      AsyncMock(return_value=(ctx, adm))):
        skill_admission_cmd.run_skill_command("list-staged")
    assert "no staged skill candidates" in capsys.readouterr().out
    adm.list_staged.assert_awaited_once()
    ctx.storage.close.assert_awaited_once()


def test_run_skill_command_list_staged_with_rows(capsys):
    ctx, adm = _fake_admission()
    adm.list_staged.return_value = [
        SimpleNamespace(id="cand-1", operation="create", risk="high",
                        skill_name="newskill", source="reviewer"),
    ]
    with patch.object(skill_admission_cmd, "_build_admission",
                      AsyncMock(return_value=(ctx, adm))):
        skill_admission_cmd.run_skill_command("list-staged")
    out = capsys.readouterr().out
    assert "cand-1" in out and "newskill" in out


def test_run_skill_command_approve(capsys):
    ctx, adm = _fake_admission()
    with patch.object(skill_admission_cmd, "_build_admission",
                      AsyncMock(return_value=(ctx, adm))):
        skill_admission_cmd.run_skill_command("approve", candidate_id="cand-1")
    assert "written: ok" in capsys.readouterr().out
    adm.approve.assert_awaited_once_with("cand-1")
    ctx.storage.close.assert_awaited_once()


def test_run_skill_command_reject(capsys):
    ctx, adm = _fake_admission()
    with patch.object(skill_admission_cmd, "_build_admission",
                      AsyncMock(return_value=(ctx, adm))):
        skill_admission_cmd.run_skill_command("reject", candidate_id="cand-1", reason="bad")
    assert "rejected: no" in capsys.readouterr().out
    adm.reject.assert_awaited_once_with("cand-1", "bad")


def test_run_skill_command_approve_without_id_exits(capsys):
    with patch.object(skill_admission_cmd, "_build_admission", AsyncMock()) as build:
        with pytest.raises(SystemExit) as exc:
            skill_admission_cmd.run_skill_command("approve")
    assert exc.value.code == 1
    build.assert_not_called()


def test_run_skill_command_reject_without_id_exits(capsys):
    with patch.object(skill_admission_cmd, "_build_admission", AsyncMock()) as build:
        with pytest.raises(SystemExit) as exc:
            skill_admission_cmd.run_skill_command("reject")
    assert exc.value.code == 1
    build.assert_not_called()


def test_run_skill_command_unknown_action_exits(capsys):
    with pytest.raises(SystemExit) as exc:
        skill_admission_cmd.run_skill_command("frobnicate")
    assert exc.value.code == 1
    assert "Unknown skill action" in capsys.readouterr().out
