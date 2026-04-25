"""Manage echo-agent as a systemd service."""

from __future__ import annotations

import getpass
import os
import shutil
import subprocess
import sys
from pathlib import Path

_SERVICE_NAME = "echo-agent"
_SERVICE_PATH = Path(f"/etc/systemd/system/{_SERVICE_NAME}.service")

_SERVICE_TEMPLATE = """\
[Unit]
Description=Echo Agent — modular AI agent framework
After=network.target

[Service]
Type=simple
User={user}
WorkingDirectory={workdir}
ExecStart={exec_start}
Restart=on-failure
RestartSec=5
KillSignal=SIGTERM
TimeoutStopSec=30
StandardOutput=journal
StandardError=journal
SyslogIdentifier=echo-agent

[Install]
WantedBy=multi-user.target
"""


def _check_linux() -> bool:
    if sys.platform != "linux":
        print("systemd service management is only supported on Linux.")
        return False
    return True


def _find_exec() -> str:
    path = shutil.which("echo-agent")
    if path:
        return f"{path} run"
    venv = os.environ.get("VIRTUAL_ENV")
    if venv:
        candidate = Path(venv) / "bin" / "echo-agent"
        if candidate.exists():
            return f"{candidate} run"
    return f"{sys.executable} -m echo_agent run"


def _run(cmd: list[str], check: bool = True) -> int:
    result = subprocess.run(cmd)
    if check and result.returncode != 0:
        sys.exit(result.returncode)
    return result.returncode


def _sudo(cmd: list[str], check: bool = True) -> int:
    if os.geteuid() != 0:
        cmd = ["sudo"] + cmd
    return _run(cmd, check=check)


def install(workspace: str | None = None) -> None:
    if not _check_linux():
        return
    workdir = Path(workspace or "~/.echo-agent").expanduser().resolve()
    content = _SERVICE_TEMPLATE.format(
        user=getpass.getuser(),
        workdir=workdir,
        exec_start=_find_exec(),
    )
    tmp = Path(f"/tmp/{_SERVICE_NAME}.service")
    tmp.write_text(content)
    _sudo(["cp", str(tmp), str(_SERVICE_PATH)])
    tmp.unlink()
    _sudo(["systemctl", "daemon-reload"])
    _sudo(["systemctl", "enable", _SERVICE_NAME])
    print(f"Service installed: {_SERVICE_PATH}")
    print(f"  ExecStart:        {_find_exec()}")
    print(f"  WorkingDirectory: {workdir}")
    print()
    print("Start it with: echo-agent service start")


def uninstall() -> None:
    if not _check_linux():
        return
    if not _SERVICE_PATH.exists():
        print("Service is not installed.")
        return
    _sudo(["systemctl", "stop", _SERVICE_NAME], check=False)
    _sudo(["systemctl", "disable", _SERVICE_NAME], check=False)
    _sudo(["rm", "-f", str(_SERVICE_PATH)])
    _sudo(["systemctl", "daemon-reload"])
    print("Service uninstalled.")


def start() -> None:
    if not _check_linux():
        return
    _sudo(["systemctl", "start", _SERVICE_NAME])
    print("Service started.")


def stop() -> None:
    if not _check_linux():
        return
    _sudo(["systemctl", "stop", _SERVICE_NAME])
    print("Service stopped.")


def restart() -> None:
    if not _check_linux():
        return
    _sudo(["systemctl", "restart", _SERVICE_NAME])
    print("Service restarted.")


def status() -> None:
    if not _check_linux():
        return
    _sudo(["systemctl", "status", _SERVICE_NAME], check=False)


def logs() -> None:
    if not _check_linux():
        return
    try:
        _sudo(["journalctl", "-u", _SERVICE_NAME, "-f", "--no-pager"])
    except KeyboardInterrupt:
        pass


def run_action(action: str, workspace: str | None = None) -> None:
    actions = {
        "install": lambda: install(workspace),
        "uninstall": uninstall,
        "start": start,
        "stop": stop,
        "restart": restart,
        "status": status,
        "logs": logs,
    }
    fn = actions.get(action)
    if fn is None:
        print(f"Unknown action: {action}")
        print(f"Available: {', '.join(actions)}")
        sys.exit(1)
    fn()
