"""systemd backends: user scope (default, no sudo) and system scope.

User scope writes ``~/.config/systemd/user/echo-agent.service`` and drives
``systemctl --user``. System scope keeps the legacy behaviour: unit under
``/etc/systemd/system`` managed via sudo.
"""

from __future__ import annotations

import getpass
import os
import shutil
import subprocess
import sys
from pathlib import Path

from echo_agent.cli.service.base import SERVICE_NAME, gateway_argv, run
from echo_agent.cli.service.templates import render_systemd_unit


def _workdir(workspace: str | None) -> Path:
    return Path(workspace or "~/.echo-agent").expanduser().resolve()


class SystemdUserBackend:
    name = "systemd-user"

    def service_path(self) -> Path:
        return Path.home() / ".config" / "systemd" / "user" / f"{SERVICE_NAME}.service"

    def _systemctl(self, args: list[str], check: bool = True) -> int:
        return run(["systemctl", "--user", *args], check=check)

    def _render(self, workspace: str | None = None) -> str:
        return render_systemd_unit(
            argv=gateway_argv(workspace),
            workdir=str(_workdir(workspace)),
        )

    def install(self, workspace: str | None = None, force: bool = False) -> None:
        unit_path = self.service_path()
        if unit_path.exists() and not force and self.is_current(workspace):
            print(f"Service already installed and up to date: {unit_path}")
            print("Start it with: echo-agent gateway start")
            return
        unit_path.parent.mkdir(parents=True, exist_ok=True)
        unit_path.write_text(self._render(workspace), encoding="utf-8")
        self._systemctl(["daemon-reload"])
        self._systemctl(["enable", SERVICE_NAME], check=False)
        print(f"Service installed: {unit_path}")
        print()
        print("Start it with: echo-agent gateway start")
        print("To keep it running after logout: sudo loginctl enable-linger " + getpass.getuser())

    def uninstall(self) -> None:
        unit_path = self.service_path()
        if not unit_path.exists():
            print("Service is not installed.")
            return
        self._systemctl(["stop", SERVICE_NAME], check=False)
        self._systemctl(["disable", SERVICE_NAME], check=False)
        unit_path.unlink()
        self._systemctl(["daemon-reload"], check=False)
        print("Service uninstalled.")

    def start(self) -> None:
        if not self.is_installed():
            print("Service is not installed. Run: echo-agent gateway install")
            raise SystemExit(1)
        self._systemctl(["start", SERVICE_NAME])
        print("Gateway service started.")
        print(f"  Logs: journalctl --user -u {SERVICE_NAME} -f")

    def stop(self) -> None:
        self._systemctl(["stop", SERVICE_NAME], check=False)
        print("Gateway service stopped.")

    def restart(self) -> None:
        if not self.is_installed():
            print("Service is not installed. Run: echo-agent gateway install")
            raise SystemExit(1)
        self._systemctl(["restart", SERVICE_NAME])
        print("Gateway service restarted.")

    def is_installed(self) -> bool:
        return self.service_path().exists()

    def is_running(self) -> bool:
        result = subprocess.run(
            ["systemctl", "--user", "is-active", "--quiet", SERVICE_NAME],
        )
        return result.returncode == 0

    def is_current(self, workspace: str | None = None) -> bool:
        unit_path = self.service_path()
        if not unit_path.exists():
            return False
        try:
            installed = unit_path.read_text(encoding="utf-8")
        except OSError:
            return False
        return installed == self._render(workspace)

    def status(self) -> None:
        if not self.is_installed():
            print("Service is not installed.")
            return
        run(["systemctl", "--user", "status", SERVICE_NAME, "--no-pager"], check=False)
        if not self.is_current():
            print("⚠ Installed unit is stale — rerun: echo-agent gateway install --force")

    def logs(self, follow: bool = False) -> None:
        cmd = ["journalctl", "--user", "-u", SERVICE_NAME, "--no-pager"]
        if follow:
            cmd.append("-f")
        else:
            cmd += ["-n", "100"]
        try:
            run(cmd, check=False)
        except KeyboardInterrupt:
            pass


class SystemdSystemBackend:
    """System-scope unit for always-on servers. Requires sudo."""

    name = "systemd-system"

    def service_path(self) -> Path:
        return Path(f"/etc/systemd/system/{SERVICE_NAME}.service")

    def _sudo(self, cmd: list[str], check: bool = True) -> int:
        if os.geteuid() != 0:
            cmd = ["sudo", *cmd]
        return run(cmd, check=check)

    def _render(self, workspace: str | None = None) -> str:
        return render_systemd_unit(
            argv=gateway_argv(workspace),
            workdir=str(_workdir(workspace)),
            user=getpass.getuser(),
        )

    def install(self, workspace: str | None = None, force: bool = False) -> None:
        unit_path = self.service_path()
        if unit_path.exists() and not force and self.is_current(workspace):
            print(f"Service already installed and up to date: {unit_path}")
            print("Start it with: echo-agent gateway start --system")
            return
        content = self._render(workspace)
        tmp = Path(f"/tmp/{SERVICE_NAME}.service")
        tmp.write_text(content, encoding="utf-8")
        self._sudo(["cp", str(tmp), str(unit_path)])
        tmp.unlink()
        self._sudo(["systemctl", "daemon-reload"])
        self._sudo(["systemctl", "enable", SERVICE_NAME])
        print(f"Service installed: {unit_path}")
        print()
        print("Start it with: echo-agent gateway start --system")

    def uninstall(self) -> None:
        unit_path = self.service_path()
        if not unit_path.exists():
            print("Service is not installed.")
            return
        self._sudo(["systemctl", "stop", SERVICE_NAME], check=False)
        self._sudo(["systemctl", "disable", SERVICE_NAME], check=False)
        self._sudo(["rm", "-f", str(unit_path)])
        self._sudo(["systemctl", "daemon-reload"])
        print("Service uninstalled.")

    def start(self) -> None:
        if not self.is_installed():
            print("Service is not installed. Run: echo-agent gateway install --system")
            raise SystemExit(1)
        self._sudo(["systemctl", "start", SERVICE_NAME])
        print("Gateway service started.")

    def stop(self) -> None:
        self._sudo(["systemctl", "stop", SERVICE_NAME], check=False)
        print("Gateway service stopped.")

    def restart(self) -> None:
        if not self.is_installed():
            print("Service is not installed. Run: echo-agent gateway install --system")
            raise SystemExit(1)
        self._sudo(["systemctl", "restart", SERVICE_NAME])
        print("Gateway service restarted.")

    def is_installed(self) -> bool:
        return self.service_path().exists()

    def is_running(self) -> bool:
        result = subprocess.run(["systemctl", "is-active", "--quiet", SERVICE_NAME])
        return result.returncode == 0

    def is_current(self, workspace: str | None = None) -> bool:
        unit_path = self.service_path()
        if not unit_path.exists():
            return False
        try:
            installed = unit_path.read_text(encoding="utf-8")
        except OSError:
            return False
        return installed == self._render(workspace)

    def status(self) -> None:
        if not self.is_installed():
            print("Service is not installed.")
            return
        self._sudo(["systemctl", "status", SERVICE_NAME, "--no-pager"], check=False)
        if not self.is_current():
            print("⚠ Installed unit is stale — rerun: echo-agent gateway install --system --force")

    def logs(self, follow: bool = False) -> None:
        cmd = ["journalctl", "-u", SERVICE_NAME, "--no-pager"]
        if follow:
            cmd.append("-f")
        else:
            cmd += ["-n", "100"]
        try:
            self._sudo(cmd, check=False)
        except KeyboardInterrupt:
            pass


def systemd_available() -> bool:
    if sys.platform != "linux":
        return False
    if not shutil.which("systemctl"):
        return False
    # WSL and containers often ship systemctl without a running systemd.
    result = subprocess.run(
        ["systemctl", "is-system-running"],
        capture_output=True, text=True,
    )
    token = (result.stdout or "").strip()
    return token not in ("", "offline", "unknown") or result.returncode == 0
