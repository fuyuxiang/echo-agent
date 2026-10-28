# tests/cli/test_cron_cmd.py
"""CLI cron authorize/revoke — the human path back to an authorized job."""
from pathlib import Path
from unittest.mock import MagicMock

from echo_agent.scheduler.authorization import grant, verify
from echo_agent.scheduler.service import ScheduledJob, TriggerKind


def _job() -> ScheduledJob:
    return ScheduledJob(
        id="j1", name="nightly", trigger=TriggerKind.CRON, cron_expr="0 9 * * *",
        payload={"command": "echo hi", "deliver_channel": "telegram", "deliver_chat_id": "1"},
    )


def test_authorize_grants_when_confirmed(monkeypatch):
    from echo_agent.cli import cron_cmd

    job = _job()
    scheduler = MagicMock()
    scheduler.get_job = MagicMock(return_value=job)
    applied = {}
    scheduler.update_job = MagicMock(
        side_effect=lambda job_id, **kw: applied.update(kw) or job
    )
    monkeypatch.setattr(cron_cmd, "_load_scheduler", lambda *a, **k: (scheduler, Path("/ws")))

    rc = cron_cmd.run_cron_command(
        "authorize", "j1", config_path=None, workspace=None, assume_yes=True
    )

    assert rc == 0
    assert applied["authorization"] is not None
    assert applied["authorization"].source == "cli"
    # update_job only writes the field when told to explicitly; without this the
    # grant is computed, reported as success, and silently dropped.
    assert applied["set_authorization"] is True
    job.authorization = applied["authorization"]
    assert verify(job) is True


def test_authorize_aborts_without_confirmation(monkeypatch, capsys):
    """The confirmation prompt is the whole point — declining must not grant."""
    from echo_agent.cli import cron_cmd

    scheduler = MagicMock()
    scheduler.get_job = MagicMock(return_value=_job())
    scheduler.update_job = MagicMock()
    monkeypatch.setattr(cron_cmd, "_load_scheduler", lambda *a, **k: (scheduler, Path("/ws")))
    monkeypatch.setattr("builtins.input", lambda *_: "n")

    rc = cron_cmd.run_cron_command(
        "authorize", "j1", config_path=None, workspace=None, assume_yes=False
    )

    assert rc == 1
    scheduler.update_job.assert_not_called()


def test_authorize_prints_instruction_and_target(monkeypatch, capsys):
    """A human cannot consent to work they were not shown."""
    from echo_agent.cli import cron_cmd

    scheduler = MagicMock()
    scheduler.get_job = MagicMock(return_value=_job())
    scheduler.update_job = MagicMock(return_value=_job())
    monkeypatch.setattr(cron_cmd, "_load_scheduler", lambda *a, **k: (scheduler, Path("/ws")))

    cron_cmd.run_cron_command("authorize", "j1", config_path=None, workspace=None, assume_yes=True)

    out = capsys.readouterr().out
    assert "echo hi" in out
    assert "0 9 * * *" in out
    assert "telegram" in out


def test_revoke_clears_authorization(monkeypatch):
    from echo_agent.cli import cron_cmd

    job = _job()
    job.authorization = grant(job, operator="alice", source="cli")
    scheduler = MagicMock()
    scheduler.get_job = MagicMock(return_value=job)
    applied = {}
    scheduler.update_job = MagicMock(side_effect=lambda job_id, **kw: applied.update(kw) or job)
    monkeypatch.setattr(cron_cmd, "_load_scheduler", lambda *a, **k: (scheduler, Path("/ws")))

    rc = cron_cmd.run_cron_command("revoke", "j1", config_path=None, workspace=None, assume_yes=True)

    assert rc == 0
    assert applied["authorization"] is None
    # authorization=None alone is indistinguishable from "leave it alone", so the
    # flag is what actually clears the grant.
    assert applied["set_authorization"] is True


def test_missing_job_returns_error(monkeypatch):
    from echo_agent.cli import cron_cmd

    scheduler = MagicMock()
    scheduler.get_job = MagicMock(return_value=None)
    monkeypatch.setattr(cron_cmd, "_load_scheduler", lambda *a, **k: (scheduler, Path("/ws")))

    rc = cron_cmd.run_cron_command("authorize", "nope", config_path=None, workspace=None, assume_yes=True)
    assert rc == 1


def test_list_shows_authorization_state(monkeypatch, capsys):
    from echo_agent.cli import cron_cmd

    authorized = _job()
    authorized.authorization = grant(authorized, operator="alice", source="cli")
    stale = _job()
    stale.id = "j2"
    stale.authorization = grant(stale, operator="bob", source="cli")
    stale.payload["command"] = "edited after grant"
    plain = _job()
    plain.id = "j3"

    scheduler = MagicMock()
    scheduler.list_jobs = MagicMock(return_value=[authorized, stale, plain])
    monkeypatch.setattr(cron_cmd, "_load_scheduler", lambda *a, **k: (scheduler, Path("/ws")))

    rc = cron_cmd.run_cron_command("list", "", config_path=None, workspace=None, assume_yes=True)

    assert rc == 0
    out = capsys.readouterr().out
    assert "已授权" in out
    assert "需要重新授权" in out
    assert "未授权" in out


def _running_gateway(monkeypatch, cron_cmd):
    """Pretend a live agent holds the workspace lock, without building one."""
    monkeypatch.setattr(cron_cmd, "_gateway_is_running", lambda _ws: True)


def test_authorize_refuses_while_gateway_runs(monkeypatch, capsys):
    """A running agent rewrites the whole store, so an offline grant is a lie."""
    from echo_agent.cli import cron_cmd

    scheduler = MagicMock()
    scheduler.get_job = MagicMock(return_value=_job())
    scheduler.update_job = MagicMock()
    monkeypatch.setattr(cron_cmd, "_load_scheduler", lambda *a, **k: (scheduler, Path("/ws")))
    _running_gateway(monkeypatch, cron_cmd)

    rc = cron_cmd.run_cron_command(
        "authorize", "j1", config_path=None, workspace=None, assume_yes=True
    )

    assert rc == 1
    scheduler.update_job.assert_not_called()
    assert "Gateway 正在运行" in capsys.readouterr().out


def test_revoke_refuses_while_gateway_runs(monkeypatch, capsys):
    """The fail-open case: the running instance would write the grant back."""
    from echo_agent.cli import cron_cmd

    job = _job()
    job.authorization = grant(job, operator="alice", source="cli")
    scheduler = MagicMock()
    scheduler.get_job = MagicMock(return_value=job)
    scheduler.update_job = MagicMock()
    monkeypatch.setattr(cron_cmd, "_load_scheduler", lambda *a, **k: (scheduler, Path("/ws")))
    _running_gateway(monkeypatch, cron_cmd)

    rc = cron_cmd.run_cron_command(
        "revoke", "j1", config_path=None, workspace=None, assume_yes=True
    )

    assert rc == 1
    scheduler.update_job.assert_not_called()
    assert "Gateway 正在运行" in capsys.readouterr().out


def test_list_still_works_while_gateway_runs(monkeypatch):
    """list only reads, so the write guard must not block it."""
    from echo_agent.cli import cron_cmd

    scheduler = MagicMock()
    scheduler.list_jobs = MagicMock(return_value=[_job()])
    monkeypatch.setattr(cron_cmd, "_load_scheduler", lambda *a, **k: (scheduler, Path("/ws")))
    _running_gateway(monkeypatch, cron_cmd)

    rc = cron_cmd.run_cron_command("list", "", config_path=None, workspace=None, assume_yes=True)

    assert rc == 0


def test_describe_falls_back_to_session_key_target(monkeypatch, capsys):
    """delivery derives the target from source_session_key; the prompt must agree.

    Showing "no delivery target" for a job that does deliver is the dangerous
    direction: the operator consents believing the output stays local.
    """
    from echo_agent.cli import cron_cmd

    job = _job()
    job.payload = {"command": "echo hi", "source_session_key": "telegram:555:777"}
    scheduler = MagicMock()
    scheduler.get_job = MagicMock(return_value=job)
    scheduler.update_job = MagicMock(return_value=job)
    monkeypatch.setattr(cron_cmd, "_load_scheduler", lambda *a, **k: (scheduler, Path("/ws")))

    cron_cmd.run_cron_command("authorize", "j1", config_path=None, workspace=None, assume_yes=True)

    out = capsys.readouterr().out
    assert "telegram" in out
    assert "555" in out
    assert "无投递目标" not in out


def test_authorize_treats_missing_stdin_as_decline(monkeypatch, capsys):
    """No stdin must read as "no", not as an uncaught EOFError traceback."""
    from echo_agent.cli import cron_cmd

    scheduler = MagicMock()
    scheduler.get_job = MagicMock(return_value=_job())
    scheduler.update_job = MagicMock()
    monkeypatch.setattr(cron_cmd, "_load_scheduler", lambda *a, **k: (scheduler, Path("/ws")))

    def _no_stdin(*_):
        raise EOFError

    monkeypatch.setattr("builtins.input", _no_stdin)

    rc = cron_cmd.run_cron_command(
        "authorize", "j1", config_path=None, workspace=None, assume_yes=False
    )

    assert rc == 1
    scheduler.update_job.assert_not_called()
    assert "已取消" in capsys.readouterr().out
