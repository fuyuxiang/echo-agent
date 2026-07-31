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

def _fallback_hints() -> str:
    """Guidance for platforms with no supported service manager.

    WSL without `systemd=true` is the common case and has an extra option the
    generic text hid: turning systemd on. A container cannot do that, so the
    two must not share one message."""
    from echo_agent.cli.runtime_probe import is_wsl

    lines = ["No supported service manager found on this platform."]
    if is_wsl():
        lines += [
            "",
            "On WSL2 you can enable systemd and register a real service:",
            "  1. Add to /etc/wsl.conf:   [boot]",
            "                             systemd=true",
            "  2. Run from Windows:       wsl --shutdown",
            "  3. Then:                   echo-agent gateway install",
        ]
    lines += [
        "",
        "Or run the gateway in the foreground:",
        "",
        "  echo-agent gateway                                    # direct foreground",
        "  tmux new -s echo-agent 'echo-agent gateway'           # persistent via tmux",
        "  nohup echo-agent gateway > ~/.echo-agent/logs/gateway.log 2>&1 &  # background",
    ]
    return "\n".join(lines)


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
    """No service installed: probe the running gateway so a manually started
    foreground instance is still reported.

    Prefers the runtime-endpoint file the gateway writes into its workspace —
    that carries the *actually bound* port, so a ``gateway.port=0`` (ephemeral)
    instance is now discoverable instead of "cannot probe". Falls back to the
    configured host/port when no endpoint file exists."""
    print("Gateway service is not installed.")
    host = "127.0.0.1"
    port = 58123
    try:
        from echo_agent.cli.workspace import (
            read_runtime_endpoint,
            resolve_effective_workspace,
        )
        from echo_agent.config.loader import load_config, resolve_config_file

        config_file = resolve_config_file(None)
        config = load_config(config_path=config_file)
        host, port = config.gateway.host, config.gateway.port
        ws = resolve_effective_workspace(
            config, str(config_file) if config_file else None, None
        )
        endpoint = read_runtime_endpoint(ws)
        if endpoint and endpoint.get("port"):
            host, port = endpoint.get("host", host), int(endpoint["port"])
    except Exception:
        pass
    if not port:
        # port=0 with no endpoint file — the gateway isn't running (a live one
        # would have written its real port), so there is nothing to probe.
        print("Gateway port is dynamic (gateway.port=0) and no running instance was found.")
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


def _resolve_install_paths(
    workspace: str | None, config: str | None
) -> tuple[str, str]:
    """Freeze absolute (workspace, config) for the supervised service.

    A daemon started by launchd/systemd inherits neither the install-time cwd
    nor a project-local config — without baking absolute paths it falls back to
    ``~/.echo-agent`` and loads a *different* config than the foreground
    ``gateway`` would. Resolve both through the same rules the runtime uses
    (:func:`resolve_config_file` + :func:`resolve_effective_workspace`) so the
    installed service is pinned regardless of where it is later launched from.
    Returns ``str`` paths ready to embed in the service file."""
    from echo_agent.cli.workspace import resolve_effective_workspace
    from echo_agent.config.loader import load_config, resolve_config_file

    config_file = resolve_config_file(config, search_dir=workspace)
    overrides = {"workspace": workspace} if workspace else None
    loaded = load_config(config_path=config_file, overrides=overrides)
    ws = resolve_effective_workspace(
        loaded, str(config_file) if config_file else None, workspace
    )
    return str(ws), (str(config_file) if config_file else "")


def run_service_action(
    action: str,
    workspace: str | None = None,
    system: bool = False,
    force: bool = False,
    follow: bool = False,
    config: str | None = None,
) -> int:
    """Run one service lifecycle action and return a process exit code."""
    if action in ("stop", "restart", "uninstall") and _refuse_inside_gateway(action):
        return 1

    backend = detect_backend(system=system)
    if backend is None:
        if action == "status":
            _status_without_service()
            return 0
        print(_fallback_hints())
        return 1

    if action == "install":
        # 固化绝对 workspace/config,使后台服务不依赖 cwd 或 ~/.echo-agent 兜底,
        # 加载与前台 gateway 完全一致的配置与工作目录。
        abs_ws, abs_config = _resolve_install_paths(workspace, config)
        backend.install(workspace=abs_ws, force=force, config=abs_config or None)
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
        return 1
    return 0


def run_action(action: str, workspace: str | None = None) -> int:
    """Deprecated shim for ``echo-agent service <action>`` (and install.sh).

    Old behaviour was Linux system-scope systemd; keep that mapping so
    existing installs keep managing the same unit.
    """
    print(
        "`echo-agent service` is deprecated; use `echo-agent gateway "
        f"{action}` instead (system-scope on Linux: add --system).",
        file=sys.stderr,
    )
    return run_service_action(
        action, workspace=workspace, system=sys.platform == "linux"
    )
