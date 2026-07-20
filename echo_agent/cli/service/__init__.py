"""Service management dispatch — ``echo-agent gateway <action>``.

Backend selection: macOS → LaunchAgent; Linux with a live systemd → user
scope by default, ``--system`` for the legacy root-managed unit. Everything
else (Windows, WSL without systemd, containers) gets actionable guidance
instead of a hard error.
"""

from __future__ import annotations

import sys

from echo_agent.cli.service.base import GATEWAY_ENV_FLAG, ServiceBackend

ACTIONS = ("install", "uninstall", "start", "stop", "restart", "status", "logs")

_FALLBACK_HINTS = """\
No supported service manager found on this platform.
Run the gateway in the foreground instead:

  echo-agent gateway                                    # direct foreground
  tmux new -s echo-agent 'echo-agent gateway'           # persistent via tmux
  nohup echo-agent gateway > ~/.echo-agent/logs/gateway.log 2>&1 &  # background
"""


def detect_backend(system: bool = False) -> ServiceBackend | None:
    if sys.platform == "darwin":
        if system:
            print("--system is not supported on macOS; a user LaunchAgent is used instead.")
        from echo_agent.cli.service.launchd import LaunchdBackend
        return LaunchdBackend()
    if sys.platform == "linux":
        from echo_agent.cli.service.systemd import (
            SystemdSystemBackend,
            SystemdUserBackend,
            systemd_available,
        )
        if not systemd_available():
            return None
        return SystemdSystemBackend() if system else SystemdUserBackend()
    return None


def _refuse_inside_gateway(action: str) -> bool:
    """Refuse stop/restart/uninstall issued from inside the gateway process
    itself (e.g. the agent's exec tool) — with KeepAlive/Restart=always this
    would otherwise produce a kill/respawn loop."""
    import os

    if os.environ.get(GATEWAY_ENV_FLAG) != "1":
        return False
    print(
        f"Refusing to {action} the gateway from inside the gateway process.\n"
        f"Run `echo-agent gateway {action}` from a shell outside the gateway.",
        file=sys.stderr,
    )
    return True


def _status_without_service() -> None:
    """No service installed: probe the configured port so a manually started
    foreground gateway is still reported."""
    print("Gateway service is not installed.")
    try:
        from echo_agent.config.loader import load_config, resolve_config_file
        config = load_config(config_path=resolve_config_file(None))
        host, port = config.gateway.host, config.gateway.port
    except Exception:
        host, port = "127.0.0.1", 58123
    if not port:
        # port=0 is the ephemeral sentinel — nothing meaningful to probe.
        print("Gateway port is dynamic (gateway.port=0); cannot probe for a running instance.")
        print()
        print("To run in the foreground:  echo-agent gateway")
        print("To install as a service:   echo-agent gateway install")
        return
    import socket

    probe_host = "127.0.0.1" if host in ("", "0.0.0.0", "::") else host
    family = socket.AF_INET6 if ":" in probe_host else socket.AF_INET
    with socket.socket(family, socket.SOCK_STREAM) as sock:
        sock.settimeout(1)
        occupied = sock.connect_ex((probe_host, port)) == 0
    if occupied:
        print(f"✓ A gateway appears to be listening on {probe_host}:{port} (running manually, not as a service).")
    else:
        print(f"✗ Nothing is listening on {probe_host}:{port}.")
    print()
    print("To run in the foreground:  echo-agent gateway")
    print("To install as a service:   echo-agent gateway install")


def run_service_action(
    action: str,
    workspace: str | None = None,
    system: bool = False,
    force: bool = False,
    follow: bool = False,
    config: str | None = None,
) -> None:
    if action in ("stop", "restart", "uninstall") and _refuse_inside_gateway(action):
        sys.exit(1)

    backend = detect_backend(system=system)
    if backend is None:
        if action == "status":
            _status_without_service()
            return
        print(_FALLBACK_HINTS)
        sys.exit(1)

    if action == "install":
        backend.install(workspace=workspace, force=force, config=config)
    elif action == "uninstall":
        backend.uninstall()
    elif action == "start":
        backend.start()
    elif action == "stop":
        backend.stop()
    elif action == "restart":
        backend.restart()
    elif action == "status":
        if backend.is_installed():
            backend.status()
        else:
            _status_without_service()
    elif action == "logs":
        backend.logs(follow=follow)
    else:
        print(f"Unknown action: {action}")
        print(f"Available: {', '.join(ACTIONS)}")
        sys.exit(1)


def run_action(action: str, workspace: str | None = None) -> None:
    """Deprecated shim for ``echo-agent service <action>`` (and install.sh).

    Old behaviour was Linux system-scope systemd; keep that mapping so
    existing installs keep managing the same unit.
    """
    print(
        "`echo-agent service` is deprecated; use `echo-agent gateway "
        f"{action}` instead (system-scope on Linux: add --system).",
        file=sys.stderr,
    )
    run_service_action(action, workspace=workspace, system=sys.platform == "linux")
