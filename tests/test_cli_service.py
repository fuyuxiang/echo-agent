"""Tests for echo_agent.cli.service — cross-platform gateway service management.

Every external effect (subprocess, filesystem paths, platform) is mocked or
redirected into tmp_path. No real service is ever installed.
"""

from __future__ import annotations

import sys
from pathlib import Path
from unittest.mock import MagicMock, patch

import pytest

from echo_agent.cli import service
from echo_agent.cli.service import base, templates
from echo_agent.cli.service.launchd import LaunchdBackend
from echo_agent.cli.service.systemd import (
    SystemdSystemBackend,
    SystemdUserBackend,
    systemd_available,
)

_PKG = "echo_agent.cli.service"


# ── base helpers ─────────────────────────────────────────────────────────────

def test_gateway_argv_uses_module_invocation():
    argv = base.gateway_argv()
    assert argv == [sys.executable, "-m", "echo_agent", "gateway"]


def test_gateway_argv_appends_resolved_workspace(tmp_path: Path):
    argv = base.gateway_argv(workspace=str(tmp_path))
    assert argv[-2] == "-w"
    assert argv[-1] == str(tmp_path.resolve())


def test_run_exits_on_nonzero_when_check():
    proc = MagicMock(returncode=3)
    with patch(f"{_PKG}.base.subprocess.run", return_value=proc):
        with pytest.raises(SystemExit) as exc:
            base.run(["false"], check=True)
    assert exc.value.code == 3


def test_run_no_exit_when_check_false():
    proc = MagicMock(returncode=3)
    with patch(f"{_PKG}.base.subprocess.run", return_value=proc):
        assert base.run(["false"], check=False) == 3


# ── templates ────────────────────────────────────────────────────────────────

def test_launchd_plist_contains_argv_and_lifecycle_keys():
    plist = templates.render_launchd_plist(
        argv=["/usr/bin/python3", "-m", "echo_agent", "gateway"],
        workdir="/home/alice/.echo-agent",
        log_path="/home/alice/.echo-agent/logs/gateway.log",
    )
    assert "<string>/usr/bin/python3</string>" in plist
    assert "<string>gateway</string>" in plist
    assert "<key>KeepAlive</key>" in plist
    assert "<key>RunAtLoad</key>" in plist
    assert "<key>ThrottleInterval</key>" in plist
    assert "gateway.log" in plist
    assert templates.LAUNCHD_LABEL in plist


def test_launchd_plist_escapes_xml():
    plist = templates.render_launchd_plist(
        argv=["/opt/a&b/python"], workdir="/w", log_path="/l",
    )
    assert "/opt/a&amp;b/python" in plist


def test_systemd_unit_user_scope():
    unit = templates.render_systemd_unit(
        argv=["/usr/bin/python3", "-m", "echo_agent", "gateway"],
        workdir="/home/alice/.echo-agent",
    )
    assert "ExecStart=/usr/bin/python3 -m echo_agent gateway" in unit
    assert "Restart=always" in unit
    assert f"TimeoutStopSec={base.STOP_TIMEOUT_SECONDS}" in unit
    assert "WantedBy=default.target" in unit
    assert "User=" not in unit


def test_systemd_unit_caps_restart_attempts():
    """配置类永久失败(如 0.0.0.0 无 token 被网关拒绝)必须进 failed,
    而不是以 RestartSec=5 无限重启、把唯一的真实错误埋在几千条同样的日志下。"""
    unit = templates.render_systemd_unit(argv=base.gateway_argv(), workdir="/w")
    assert f"StartLimitIntervalSec={templates.START_LIMIT_INTERVAL_SECONDS}" in unit
    assert f"StartLimitBurst={templates.START_LIMIT_BURST}" in unit
    # 两个指令属于 [Unit],放进 [Service] 会被 systemd 忽略
    unit_section = unit.split("[Service]")[0]
    assert "StartLimitIntervalSec=" in unit_section
    assert "StartLimitBurst=" in unit_section


def test_systemd_unit_system_scope_sets_user_and_target():
    unit = templates.render_systemd_unit(
        argv=["/usr/bin/echo-agent", "gateway"], workdir="/srv", user="alice",
    )
    assert "User=alice" in unit
    assert "WantedBy=multi-user.target" in unit


def test_unit_gateway_not_run_entrypoint():
    """The supervised entrypoint must be `gateway`, never `run` — `run`
    exits immediately without a TTY or active channels."""
    unit = templates.render_systemd_unit(argv=base.gateway_argv(), workdir="/w")
    assert " gateway" in unit
    assert not unit.split("ExecStart=")[1].splitlines()[0].endswith(" run")


# ── detect_backend ───────────────────────────────────────────────────────────

def test_detect_backend_macos():
    with patch(f"{_PKG}.sys") as sysmod:
        sysmod.platform = "darwin"
        backend = service.detect_backend()
    assert isinstance(backend, LaunchdBackend)


def test_detect_backend_linux_user_default():
    with patch(f"{_PKG}.sys") as sysmod, \
         patch(f"{_PKG}.systemd.systemd_available", return_value=True):
        sysmod.platform = "linux"
        backend = service.detect_backend()
    assert isinstance(backend, SystemdUserBackend)


def test_detect_backend_linux_system_flag():
    with patch(f"{_PKG}.sys") as sysmod, \
         patch(f"{_PKG}.systemd.systemd_available", return_value=True):
        sysmod.platform = "linux"
        backend = service.detect_backend(system=True)
    assert isinstance(backend, SystemdSystemBackend)


def test_detect_backend_linux_no_systemd():
    with patch(f"{_PKG}.sys") as sysmod, \
         patch(f"{_PKG}.systemd.systemd_available", return_value=False):
        sysmod.platform = "linux"
        assert service.detect_backend() is None


def test_detect_backend_unsupported_platform():
    with patch(f"{_PKG}.sys") as sysmod:
        sysmod.platform = "win32"
        assert service.detect_backend() is None


def test_systemd_available_false_without_systemctl():
    with patch(f"{_PKG}.systemd.sys") as sysmod, \
         patch(f"{_PKG}.systemd.shutil.which", return_value=None):
        sysmod.platform = "linux"
        assert systemd_available() is False


# ── suicide guard ────────────────────────────────────────────────────────────

@pytest.mark.parametrize("action", ["stop", "restart", "uninstall"])
def test_refuses_lifecycle_from_inside_gateway(action, monkeypatch, capsys):
    monkeypatch.setenv(base.GATEWAY_ENV_FLAG, "1")
    # 退出码由 run_service_action 返回,sys.exit 归 __main__ 统一负责。
    assert service.run_service_action(action) == 1
    assert "inside the gateway" in capsys.readouterr().err


def test_allows_status_from_inside_gateway(monkeypatch):
    monkeypatch.setenv(base.GATEWAY_ENV_FLAG, "1")
    backend = MagicMock()
    backend.is_installed.return_value = True
    with patch(f"{_PKG}.detect_backend", return_value=backend):
        service.run_service_action("status")
    backend.status.assert_called_once()


# ── run_service_action dispatch ──────────────────────────────────────────────

@pytest.mark.parametrize(
    "action,method,kwargs",
    [
        ("uninstall", "uninstall", {}),
        ("start", "start", {}),
        ("stop", "stop", {}),
        ("restart", "restart", {}),
        ("logs", "logs", {"follow": False}),
    ],
)
def test_run_service_action_dispatches(action, method, kwargs, monkeypatch):
    monkeypatch.delenv(base.GATEWAY_ENV_FLAG, raising=False)
    backend = MagicMock()
    with patch(f"{_PKG}.detect_backend", return_value=backend):
        service.run_service_action(action)
    getattr(backend, method).assert_called_once_with(**kwargs)


def test_install_freezes_absolute_paths(monkeypatch, tmp_path):
    # install 前解析出绝对 workspace/config,固化进服务文件,后台服务不再依赖
    # cwd 或 ~/.echo-agent 兜底(任务二)。这里断言传给 backend.install 的是绝对路径。
    monkeypatch.delenv(base.GATEWAY_ENV_FLAG, raising=False)
    backend = MagicMock()
    ws = tmp_path / "ws"
    with patch(f"{_PKG}.detect_backend", return_value=backend), \
         patch(f"{_PKG}._resolve_install_paths", return_value=(str(ws), str(tmp_path / "echo-agent.yaml"))):
        service.run_service_action("install", workspace="./ws")
    backend.install.assert_called_once_with(
        workspace=str(ws), force=False, config=str(tmp_path / "echo-agent.yaml")
    )


def test_resolve_install_paths_returns_absolute(tmp_path, monkeypatch):
    # 相对 -w 按 cwd 解析为绝对;无 config 时返回空串(install 转成 None)。
    monkeypatch.chdir(tmp_path)
    with patch("echo_agent.config.loader.resolve_config_file", return_value=None):
        abs_ws, abs_config = service._resolve_install_paths("data", None)
    assert Path(abs_ws).is_absolute()
    assert abs_ws == str((tmp_path / "data").resolve())
    assert abs_config == ""


def test_resolve_install_paths_freezes_config(tmp_path, monkeypatch):
    # 给定显式 config,固化为绝对路径。
    monkeypatch.chdir(tmp_path)
    cfg = tmp_path / "echo-agent.yaml"
    cfg.write_text("workspace: ./data\n", encoding="utf-8")
    abs_ws, abs_config = service._resolve_install_paths(None, str(cfg))
    assert abs_config == str(cfg.resolve())
    # 无 -w 时相对 workspace 按 config 文件所在目录解析。
    assert abs_ws == str((tmp_path / "data").resolve())



def test_run_service_action_no_backend_exits_with_hints(monkeypatch, capsys):
    monkeypatch.delenv(base.GATEWAY_ENV_FLAG, raising=False)
    with patch(f"{_PKG}.detect_backend", return_value=None):
        assert service.run_service_action("install") == 1
    out = capsys.readouterr().out
    assert "tmux" in out and "nohup" in out


def test_run_service_action_status_falls_back_when_uninstalled(monkeypatch):
    monkeypatch.delenv(base.GATEWAY_ENV_FLAG, raising=False)
    backend = MagicMock()
    backend.is_installed.return_value = False
    with patch(f"{_PKG}.detect_backend", return_value=backend), \
         patch(f"{_PKG}._status_without_service") as fallback:
        service.run_service_action("status")
    fallback.assert_called_once()
    backend.status.assert_not_called()


# ── deprecated `service` alias ───────────────────────────────────────────────

def test_run_action_prints_deprecation_and_forwards(monkeypatch, capsys):
    monkeypatch.delenv(base.GATEWAY_ENV_FLAG, raising=False)
    with patch(f"{_PKG}.run_service_action") as forward:
        service.run_action("install", workspace="/srv/agent")
    assert "deprecated" in capsys.readouterr().err
    forward.assert_called_once()
    assert forward.call_args.kwargs["workspace"] == "/srv/agent"


# ── LaunchdBackend ───────────────────────────────────────────────────────────

@pytest.fixture
def launchd(tmp_path: Path, monkeypatch):
    backend = LaunchdBackend()
    monkeypatch.setattr(LaunchdBackend, "service_path", lambda self: tmp_path / "com.echo-agent.gateway.plist")
    monkeypatch.setattr(f"{_PKG}.launchd.log_file", lambda: tmp_path / "logs" / "gateway.log")
    return backend


def test_launchd_install_writes_plist(launchd, capsys):
    launchd.install()
    assert launchd.service_path().exists()
    content = launchd.service_path().read_text()
    assert "KeepAlive" in content
    assert "LaunchAgent installed" in capsys.readouterr().out


def test_launchd_install_skips_when_current(launchd, capsys):
    launchd.install()
    capsys.readouterr()
    launchd.install()
    assert "up to date" in capsys.readouterr().out


def test_launchd_install_rewrites_stale(launchd, capsys):
    launchd.install()
    launchd.service_path().write_text("<plist>old</plist>")
    capsys.readouterr()
    launchd.install()
    assert "stale" in capsys.readouterr().out
    assert "KeepAlive" in launchd.service_path().read_text()


def test_launchd_is_current_roundtrip(launchd):
    assert launchd.is_current() is False
    launchd.install()
    assert launchd.is_current() is True
    launchd.service_path().write_text("tampered")
    assert launchd.is_current() is False


def test_launchd_install_default_rewrites_custom(launchd, capsys, tmp_path):
    # 先用自定义 workspace 安装,再执行普通 install(默认参数):应检出差异并重写回
    # 默认,而非误报 up to date 保留旧参数(P4)。
    custom_ws = str(tmp_path / "custom-ws")
    launchd.install(workspace=custom_ws)
    assert custom_ws in launchd.service_path().read_text()
    capsys.readouterr()
    launchd.install()
    out = capsys.readouterr().out
    assert "stale" in out
    assert custom_ws not in launchd.service_path().read_text()


def test_launchd_status_recovers_custom_params(launchd, capsys, tmp_path):
    # status 探测对自定义参数安装不应误报 stale:从已装 plist 回读 -w/-c 自比对。
    custom_ws = str(tmp_path / "custom-ws")
    launchd.install(workspace=custom_ws)
    assert launchd._is_current_recovered() is True
    with patch(f"{_PKG}.launchd.LaunchdBackend.is_running", return_value=True):
        launchd.status()
    assert "stale" not in capsys.readouterr().out


def test_launchd_start_requires_install(launchd, capsys):
    with pytest.raises(SystemExit):
        launchd.start()
    assert "not installed" in capsys.readouterr().out


def test_launchd_start_tolerates_already_bootstrapped(launchd):
    launchd.install()
    boot = MagicMock(returncode=37, stderr="")
    with patch(f"{_PKG}.launchd.subprocess.run", return_value=boot), \
         patch(f"{_PKG}.launchd.run") as kick:
        launchd.start()
    kick.assert_called_once()
    assert kick.call_args.args[0][:2] == ["launchctl", "kickstart"]


def test_launchd_stop_uses_bootout(launchd, capsys):
    ok = MagicMock(returncode=0, stderr="")
    with patch(f"{_PKG}.launchd.subprocess.run", return_value=ok) as sp:
        launchd.stop()
    assert sp.call_args.args[0][:2] == ["launchctl", "bootout"]
    assert "stopped" in capsys.readouterr().out


def test_launchd_uninstall_removes_plist(launchd, capsys):
    launchd.install()
    with patch(f"{_PKG}.launchd.subprocess.run", return_value=MagicMock(returncode=0)):
        launchd.uninstall()
    assert not launchd.service_path().exists()
    assert "uninstalled" in capsys.readouterr().out


# ── SystemdUserBackend ───────────────────────────────────────────────────────

@pytest.fixture
def systemd_user(tmp_path: Path, monkeypatch):
    backend = SystemdUserBackend()
    monkeypatch.setattr(SystemdUserBackend, "service_path", lambda self: tmp_path / "echo-agent.service")
    return backend


def test_systemd_user_install_writes_unit_no_sudo(systemd_user, capsys):
    with patch(f"{_PKG}.systemd.run", return_value=0) as run_mock:
        systemd_user.install()
    assert systemd_user.service_path().exists()
    unit = systemd_user.service_path().read_text()
    assert "Restart=always" in unit
    calls = [c.args[0] for c in run_mock.call_args_list]
    assert ["systemctl", "--user", "daemon-reload"] in calls
    for c in calls:
        assert c[0] != "sudo"
    out = capsys.readouterr().out
    assert "enable-linger" in out


def test_systemd_user_is_current_detects_drift(systemd_user):
    with patch(f"{_PKG}.systemd.run", return_value=0):
        systemd_user.install()
    assert systemd_user.is_current() is True
    systemd_user.service_path().write_text("[Unit]\nstale")
    assert systemd_user.is_current() is False


def test_systemd_user_start_requires_install(systemd_user, capsys):
    with pytest.raises(SystemExit):
        systemd_user.start()
    assert "not installed" in capsys.readouterr().out


def test_systemd_user_uninstall(systemd_user, capsys):
    with patch(f"{_PKG}.systemd.run", return_value=0):
        systemd_user.install()
        capsys.readouterr()
        systemd_user.uninstall()
    assert not systemd_user.service_path().exists()
    assert "uninstalled" in capsys.readouterr().out


# ── SystemdSystemBackend ─────────────────────────────────────────────────────

def test_systemd_system_sudo_prepended_when_not_root():
    backend = SystemdSystemBackend()
    with patch(f"{_PKG}.systemd.os.geteuid", return_value=1000), \
         patch(f"{_PKG}.systemd.run", return_value=0) as run_mock:
        backend._sudo(["systemctl", "start", "echo-agent"])
    run_mock.assert_called_once_with(["sudo", "systemctl", "start", "echo-agent"], check=True)


def test_systemd_system_sudo_skipped_when_root():
    backend = SystemdSystemBackend()
    with patch(f"{_PKG}.systemd.os.geteuid", return_value=0), \
         patch(f"{_PKG}.systemd.run", return_value=0) as run_mock:
        backend._sudo(["systemctl", "start", "echo-agent"], check=False)
    run_mock.assert_called_once_with(["systemctl", "start", "echo-agent"], check=False)


def test_systemd_system_uninstall_noop_when_absent(capsys):
    backend = SystemdSystemBackend()
    with patch.object(SystemdSystemBackend, "service_path", return_value=Path("/nonexistent/echo-agent.service")), \
         patch.object(SystemdSystemBackend, "_sudo") as sudo:
        backend.uninstall()
    sudo.assert_not_called()
    assert "not installed" in capsys.readouterr().out
