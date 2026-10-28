"""Lifecycle failures must not be reported as successful service changes."""

from __future__ import annotations

import stat
from pathlib import Path
from unittest.mock import MagicMock

import pytest

from echo_agent.cli.service import systemd
from echo_agent.cli.service.launchd import LaunchdBackend


@pytest.mark.parametrize("backend_type", [systemd.SystemdUserBackend, systemd.SystemdSystemBackend])
@pytest.mark.parametrize("action", ["stop", "uninstall"])
def test_systemd_lifecycle_failure_preserves_unit_and_exit_status(
    tmp_path, monkeypatch, capsys, backend_type, action,
):
    unit = tmp_path / "echo-agent.service"
    unit.write_text("original service")
    backend = backend_type()
    monkeypatch.setattr(backend, "service_path", lambda: unit)

    def failed_command(_argv, check=True):
        if check:
            raise SystemExit(7)
        return 7

    monkeypatch.setattr(systemd, "run", failed_command)
    with pytest.raises(SystemExit) as exc:
        getattr(backend, action)()
    assert exc.value.code == 7
    assert unit.read_text() == "original service"
    assert "stopped" not in capsys.readouterr().out


def test_user_service_enable_failure_is_visible(tmp_path, monkeypatch, capsys):
    backend = systemd.SystemdUserBackend()
    monkeypatch.setattr(backend, "service_path", lambda: tmp_path / "echo-agent.service")

    def fail_enable(argv, check=True):
        if "enable" in argv:
            if check:
                raise SystemExit(8)
            return 8
        return 0

    monkeypatch.setattr(systemd, "run", fail_enable)
    with pytest.raises(SystemExit) as exc:
        backend.install()
    assert exc.value.code == 8
    assert "Service installed:" not in capsys.readouterr().out


@pytest.mark.parametrize("copy_fails", [False, True])
def test_system_service_install_uses_private_temp_tree_and_cleans_up(
    tmp_path, monkeypatch, copy_fails,
):
    backend = systemd.SystemdSystemBackend()
    monkeypatch.setattr(backend, "service_path", lambda: tmp_path / "installed.service")
    copied_from = []

    def fake_sudo(argv, check=True):
        if argv[0] == "cp":
            source = Path(argv[1])
            copied_from.append(source)
            assert source != Path("/tmp/echo-agent.service")
            assert stat.S_IMODE(source.parent.stat().st_mode) == 0o700
            assert "ExecStart=" in source.read_text()
            if copy_fails:
                raise SystemExit(9)
        return 0

    monkeypatch.setattr(backend, "_sudo", fake_sudo)
    if copy_fails:
        with pytest.raises(SystemExit) as exc:
            backend.install()
        assert exc.value.code == 9
    else:
        backend.install()
    assert copied_from
    assert not copied_from[0].exists()
    assert not copied_from[0].parent.exists()


@pytest.mark.parametrize("action", ["start", "restart"])
def test_launchd_kickstart_failure_is_visible(tmp_path, monkeypatch, capsys, action):
    backend = LaunchdBackend()
    unit = tmp_path / "echo-agent.plist"
    unit.touch()
    monkeypatch.setattr(backend, "service_path", lambda: unit)
    monkeypatch.setattr(backend, "is_running", lambda: True)
    monkeypatch.setattr("echo_agent.cli.service.launchd.run_owned", lambda *a, **k: MagicMock(returncode=0))

    def fail_kickstart(argv, check=True):
        if check:
            raise SystemExit(10)
        return 10

    monkeypatch.setattr("echo_agent.cli.service.launchd.run", fail_kickstart)
    with pytest.raises(SystemExit) as exc:
        getattr(backend, action)()
    assert exc.value.code == 10
    assert "started" not in capsys.readouterr().out
