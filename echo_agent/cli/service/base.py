"""Service backend protocol and shared helpers.

A backend wraps one OS service manager (launchd, systemd user/system). The
managed unit always runs ``echo-agent gateway`` in the foreground — the
service manager owns backgrounding, restart-on-crash, and log capture.
"""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path
from typing import Protocol

SERVICE_NAME = "echo-agent"
GATEWAY_ENV_FLAG = "_ECHO_AGENT_GATEWAY"

# Must exceed the worst-case graceful shutdown (AppRuntime.stop: agent drain,
# channel/gateway teardown, storage close) so the supervisor never SIGKILLs
# the process while it is still reaping its own tool subprocesses.
STOP_TIMEOUT_SECONDS = 60


def echo_home() -> Path:
    from echo_agent.runtime_paths import echo_home as _home
    return _home()


def log_file() -> Path:
    return echo_home() / "logs" / "gateway.log"


def gateway_argv(workspace: str | None = None) -> list[str]:
    """Absolute-path argv for the supervised gateway process.

    ``{python} -m echo_agent gateway`` rather than the ``echo-agent`` console
    script: service environments (launchd especially) start with a minimal
    PATH that rarely contains the venv bin directory.
    """
    argv = [sys.executable, "-m", "echo_agent", "gateway"]
    if workspace:
        argv += ["-w", str(Path(workspace).expanduser().resolve())]
    return argv


def run(cmd: list[str], check: bool = True) -> int:
    result = subprocess.run(cmd)
    if check and result.returncode != 0:
        sys.exit(result.returncode)
    return result.returncode


class ServiceBackend(Protocol):
    """Uniform lifecycle surface across service managers."""

    name: str

    def service_path(self) -> Path: ...

    def install(self, workspace: str | None = None, force: bool = False) -> None: ...

    def uninstall(self) -> None: ...

    def start(self) -> None: ...

    def stop(self) -> None: ...

    def restart(self) -> None: ...

    def is_installed(self) -> bool: ...

    def is_running(self) -> bool: ...

    def is_current(self, workspace: str | None = None) -> bool:
        """Whether the installed service file matches what we would generate
        now. Drifts after upgrades (interpreter path, argv changes)."""
        ...

    def status(self) -> None: ...

    def logs(self, follow: bool = False) -> None: ...
