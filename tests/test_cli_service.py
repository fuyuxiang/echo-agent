"""Tests for echo_agent.cli.service — systemd service management.

Every external effect (subprocess, sudo, filesystem writes, euid, platform)
is mocked. No real process is ever started or stopped.
"""

from __future__ import annotations

from pathlib import Path
from unittest.mock import MagicMock, patch

import pytest

from echo_agent.cli import service

_T = "echo_agent.cli.service"


# ── _check_linux ──────────────────────────────────────────────────────────────

def test_check_linux_true_on_linux():
    with patch(f"{_T}.sys") as sysmod:
        sysmod.platform = "linux"
        assert service._check_linux() is True


def test_check_linux_false_off_linux(capsys):
    with patch(f"{_T}.sys") as sysmod:
        sysmod.platform = "darwin"
        assert service._check_linux() is False
    assert "only supported on Linux" in capsys.readouterr().out


# ── _find_exec ──────────────────────────────────────────────────────────────────

def test_find_exec_prefers_path():
    with patch(f"{_T}.shutil.which", return_value="/usr/bin/echo-agent"):
        assert service._find_exec() == "/usr/bin/echo-agent run"


def test_find_exec_falls_back_to_venv(tmp_path: Path, monkeypatch):
    venv = tmp_path / "venv"
    (venv / "bin").mkdir(parents=True)
    (venv / "bin" / "echo-agent").write_text("#!/bin/sh\n")
    monkeypatch.setenv("VIRTUAL_ENV", str(venv))
    with patch(f"{_T}.shutil.which", return_value=None):
        result = service._find_exec()
    assert result.endswith("echo-agent run")
    assert str(venv) in result


def test_find_exec_falls_back_to_module(monkeypatch):
    monkeypatch.delenv("VIRTUAL_ENV", raising=False)
    with patch(f"{_T}.shutil.which", return_value=None), \
         patch(f"{_T}.sys") as sysmod:
        sysmod.executable = "/usr/bin/python3"
        result = service._find_exec()
    assert result == "/usr/bin/python3 -m echo_agent run"


# ── _run / _sudo ──────────────────────────────────────────────────────────────

def test_run_returns_returncode_on_success():
    proc = MagicMock(returncode=0)
    with patch(f"{_T}.subprocess.run", return_value=proc) as sp:
        rc = service._run(["echo", "hi"])
    assert rc == 0
    sp.assert_called_once_with(["echo", "hi"])


def test_run_exits_on_nonzero_when_check():
    proc = MagicMock(returncode=3)
    with patch(f"{_T}.subprocess.run", return_value=proc):
        with pytest.raises(SystemExit) as exc:
            service._run(["false"], check=True)
    assert exc.value.code == 3


def test_run_no_exit_when_check_false():
    proc = MagicMock(returncode=3)
    with patch(f"{_T}.subprocess.run", return_value=proc):
        assert service._run(["false"], check=False) == 3


def test_sudo_prepends_sudo_when_not_root():
    with patch(f"{_T}.os.geteuid", return_value=1000), \
         patch(f"{_T}._run", return_value=0) as run:
        service._sudo(["systemctl", "start", "x"])
    run.assert_called_once_with(["sudo", "systemctl", "start", "x"], check=True)


def test_sudo_skips_sudo_when_root():
    with patch(f"{_T}.os.geteuid", return_value=0), \
         patch(f"{_T}._run", return_value=0) as run:
        service._sudo(["systemctl", "start", "x"], check=False)
    run.assert_called_once_with(["systemctl", "start", "x"], check=False)


# ── install / uninstall ─────────────────────────────────────────────────────────

def test_install_short_circuits_off_linux():
    with patch(f"{_T}._check_linux", return_value=False), \
         patch(f"{_T}._sudo") as sudo:
        service.install()
    sudo.assert_not_called()


def test_install_writes_unit_and_enables(tmp_path: Path, capsys, monkeypatch):
    tmp_unit = tmp_path / "echo-agent.service"
    monkeypatch.setattr(service, "_SERVICE_PATH", tmp_path / "system" / "echo-agent.service")
    real_path = service.Path

    def _path(arg):
        if str(arg).startswith("/tmp/"):
            return tmp_unit
        return real_path(arg)

    with patch(f"{_T}._check_linux", return_value=True), \
         patch(f"{_T}._find_exec", return_value="/bin/echo-agent run"), \
         patch(f"{_T}.getpass.getuser", return_value="alice"), \
         patch(f"{_T}.Path", side_effect=_path), \
         patch(f"{_T}._sudo") as sudo:
        service.install(workspace=str(tmp_path / "ws"))

    calls = [c.args[0] for c in sudo.call_args_list]
    assert ["systemctl", "daemon-reload"] in calls
    assert ["systemctl", "enable", "echo-agent"] in calls
    assert "Service installed" in capsys.readouterr().out
    # The temp unit file was created then unlinked.
    assert not tmp_unit.exists()


def test_uninstall_noop_when_absent(capsys):
    fake_path = MagicMock()
    fake_path.exists.return_value = False
    with patch(f"{_T}._check_linux", return_value=True), \
         patch(f"{_T}._SERVICE_PATH", fake_path), \
         patch(f"{_T}._sudo") as sudo:
        service.uninstall()
    sudo.assert_not_called()
    assert "not installed" in capsys.readouterr().out


def test_uninstall_removes_when_present(capsys):
    fake_path = MagicMock()
    fake_path.exists.return_value = True
    with patch(f"{_T}._check_linux", return_value=True), \
         patch(f"{_T}._SERVICE_PATH", fake_path), \
         patch(f"{_T}._sudo") as sudo:
        service.uninstall()
    calls = [c.args[0][:2] for c in sudo.call_args_list]
    assert ["systemctl", "stop"] in calls
    assert ["systemctl", "disable"] in calls
    assert "uninstalled" in capsys.readouterr().out


# ── simple lifecycle wrappers ───────────────────────────────────────────────────

@pytest.mark.parametrize(
    "fn,verb",
    [(service.start, "start"), (service.stop, "stop"), (service.restart, "restart")],
)
def test_lifecycle_wrappers(fn, verb, capsys):
    with patch(f"{_T}._check_linux", return_value=True), \
         patch(f"{_T}._sudo") as sudo:
        fn()
    sudo.assert_called_once_with(["systemctl", verb, "echo-agent"])
    assert "Service" in capsys.readouterr().out


def test_lifecycle_wrappers_short_circuit_off_linux():
    for fn in (service.start, service.stop, service.restart, service.status):
        with patch(f"{_T}._check_linux", return_value=False), \
             patch(f"{_T}._sudo") as sudo:
            fn()
        sudo.assert_not_called()


def test_status_uses_check_false():
    with patch(f"{_T}._check_linux", return_value=True), \
         patch(f"{_T}._sudo") as sudo:
        service.status()
    sudo.assert_called_once_with(["systemctl", "status", "echo-agent"], check=False)


def test_logs_swallows_keyboard_interrupt():
    with patch(f"{_T}._check_linux", return_value=True), \
         patch(f"{_T}._sudo", side_effect=KeyboardInterrupt):
        # Must not raise.
        service.logs()


# ── run_action dispatch ──────────────────────────────────────────────────────────

@pytest.mark.parametrize(
    "action,target",
    [
        ("uninstall", "uninstall"),
        ("start", "start"),
        ("stop", "stop"),
        ("restart", "restart"),
        ("status", "status"),
        ("logs", "logs"),
    ],
)
def test_run_action_dispatches(action, target):
    with patch(f"{_T}.{target}") as fn:
        service.run_action(action)
    fn.assert_called_once()


def test_run_action_install_passes_workspace():
    with patch(f"{_T}.install") as inst:
        service.run_action("install", workspace="/srv/agent")
    inst.assert_called_once_with("/srv/agent")


def test_run_action_unknown_exits(capsys):
    with pytest.raises(SystemExit) as exc:
        service.run_action("frobnicate")
    assert exc.value.code == 1
    assert "Unknown action" in capsys.readouterr().out
