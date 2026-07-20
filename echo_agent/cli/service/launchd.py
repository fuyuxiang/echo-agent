"""macOS LaunchAgent backend (user-level, no sudo).

stop() uses ``launchctl bootout`` — removes the agent from the current boot
session without persisting a disable, so KeepAlive crash-recovery still works
and the next ``start`` re-bootstraps cleanly.
"""

from __future__ import annotations

import os
import subprocess
from pathlib import Path

from echo_agent.cli.service.base import gateway_argv, log_file, run
from echo_agent.cli.service.templates import LAUNCHD_LABEL, render_launchd_plist


class LaunchdBackend:
    name = "launchd"

    def service_path(self) -> Path:
        return Path.home() / "Library" / "LaunchAgents" / f"{LAUNCHD_LABEL}.plist"

    def _domain_target(self) -> str:
        return f"gui/{os.getuid()}/{LAUNCHD_LABEL}"

    def _render(self, workspace: str | None = None, config: str | None = None) -> str:
        log_path = log_file()
        workdir = Path(workspace).expanduser().resolve() if workspace else Path.home() / ".echo-agent"
        return render_launchd_plist(
            argv=gateway_argv(workspace, config),
            workdir=str(workdir),
            log_path=str(log_path),
        )

    def _installed_target(self) -> tuple[str | None, str | None]:
        """Recover the (workspace, config) baked into the installed plist.

        status()/is_current() have no access to the args used at install time;
        without this they'd re-render with the home-default workspace and report
        a service installed under a custom -w/-c as permanently stale. Parsing
        the persisted ProgramArguments gives an accurate self-comparison."""
        import plistlib

        plist_path = self.service_path()
        try:
            data = plistlib.loads(plist_path.read_bytes())
        except (OSError, ValueError):
            return None, None
        if not isinstance(data, dict):
            return None, None
        from echo_agent.cli.service.base import parse_gateway_argv
        return parse_gateway_argv(data.get("ProgramArguments") or [])

    def install(self, workspace: str | None = None, force: bool = False, config: str | None = None) -> None:
        plist_path = self.service_path()
        if plist_path.exists() and not force:
            if self.is_current(workspace, config):
                print(f"LaunchAgent already installed and up to date: {plist_path}")
                print("Start it with: echo-agent gateway start")
                return
            print("Installed LaunchAgent is stale (paths or arguments changed); rewriting it.")
        log_file().parent.mkdir(parents=True, exist_ok=True)
        plist_path.parent.mkdir(parents=True, exist_ok=True)
        content = self._render(workspace, config)
        plist_path.write_text(content, encoding="utf-8")
        print(f"LaunchAgent installed: {plist_path}")
        print(f"  Logs: {log_file()}")
        print()
        print("Start it with: echo-agent gateway start")

    def uninstall(self) -> None:
        plist_path = self.service_path()
        if not plist_path.exists():
            print("LaunchAgent is not installed.")
            return
        subprocess.run(
            ["launchctl", "bootout", self._domain_target()],
            capture_output=True,
        )
        plist_path.unlink()
        print("LaunchAgent uninstalled.")

    def start(self) -> None:
        plist_path = self.service_path()
        if not plist_path.exists():
            print("LaunchAgent is not installed. Run: echo-agent gateway install")
            raise SystemExit(1)
        # bootstrap is idempotent-ish: already-loaded returns EALREADY (37);
        # treat that as loaded and kickstart.
        result = subprocess.run(
            ["launchctl", "bootstrap", f"gui/{os.getuid()}", str(plist_path)],
            capture_output=True, text=True,
        )
        if result.returncode not in (0, 37):
            stderr = (result.stderr or "").strip()
            print(f"launchctl bootstrap failed ({result.returncode}): {stderr}")
            raise SystemExit(result.returncode)
        run(["launchctl", "kickstart", self._domain_target()], check=False)
        print("Gateway service started.")
        print(f"  Logs: {log_file()}")

    def stop(self) -> None:
        result = subprocess.run(
            ["launchctl", "bootout", self._domain_target()],
            capture_output=True, text=True,
        )
        if result.returncode == 0:
            print("Gateway service stopped (crash auto-recovery stays enabled; `start` re-enables).")
        else:
            print("Gateway service is not running.")

    def restart(self) -> None:
        if self.is_running():
            run(["launchctl", "kickstart", "-k", self._domain_target()], check=False)
            print("Gateway service restarted.")
        else:
            self.start()

    def is_installed(self) -> bool:
        return self.service_path().exists()

    def is_running(self) -> bool:
        result = subprocess.run(
            ["launchctl", "print", self._domain_target()],
            capture_output=True,
        )
        return result.returncode == 0

    def is_current(self, workspace: str | None = None, config: str | None = None) -> bool:
        # 纯比对:把入参当作“本次期望的参数”,None 表示默认参数。install 用它判断
        # 已装内容是否等于本次请求要装的内容 —— 先用自定义 -w/-c 安装、再执行普通
        # install(参数缺省)时,能正确检出差异并重写回默认,而非误报 up to date。
        plist_path = self.service_path()
        if not plist_path.exists():
            return False
        try:
            installed = plist_path.read_text(encoding="utf-8")
        except OSError:
            return False
        return installed == self._render(workspace, config)

    def _is_current_recovered(self) -> bool:
        # status 探测专用:无从得知安装期参数,从已装 plist 回读 -w/-c 再自比对,
        # 避免把自定义参数安装的服务误判为 stale。与 is_current 的“期望参数”语义
        # 区分开:此处目的是“文件是否仍是本程序渲染的形态”,而非“是否等于默认”。
        workspace, config = self._installed_target()
        return self.is_current(workspace, config)

    def status(self) -> None:
        if not self.is_installed():
            print("LaunchAgent is not installed.")
            return
        if self.is_running():
            print(f"✓ Gateway service is running ({LAUNCHD_LABEL})")
        else:
            print(f"✗ Gateway service is installed but not running ({LAUNCHD_LABEL})")
            print("  Start it with: echo-agent gateway start")
        if not self._is_current_recovered():
            print("  ⚠ Installed LaunchAgent is stale — rerun: echo-agent gateway install --force")

    def logs(self, follow: bool = False) -> None:
        path = log_file()
        if not path.exists():
            print(f"No log file yet: {path}")
            return
        cmd = ["tail", "-f" if follow else "-n", *([] if follow else ["100"]), str(path)]
        try:
            run(cmd, check=False)
        except KeyboardInterrupt:
            pass
