"""Gateway runtime state — the single source of truth for "is the gateway
actually usable right now".

Three places used to answer this question independently: status.py probed
endpoint file + TCP + service state, setup's doctor read only the YAML (and so
printed a ✓ for a gateway nobody was serving), and attach_client hardcoded a
`systemctl` suggestion that is wrong on WSL without systemd. They disagreed.
This module is the one answer all three now call.

Read-only and non-raising by contract: it runs on the failure path of
`echo-agent cli`, where the user is already stuck. Any sub-probe that fails
degrades to "unknown" rather than adding a traceback on top.
"""

from __future__ import annotations

import socket
from dataclasses import dataclass
from enum import Enum
from pathlib import Path
from typing import Any


_WILDCARD_HOSTS = ("0.0.0.0", "::", "")
"""Bind-only wildcards. A server bound here is reachable on the loopback, but
connecting *to* the wildcard is not the same as connecting to the service, so
every probe has to translate them first."""


class GatewayState(str, Enum):
    """The five states exhaust every case, and each maps to exactly one next
    action — that is the property callers rely on to render guidance."""

    RUNNING = "running"
    SERVICE_INSTALLED_STOPPED = "service_installed_stopped"
    NOT_INSTALLED = "not_installed"
    NO_SERVICE_MANAGER = "no_service_manager"
    DISABLED = "disabled"


@dataclass(frozen=True)
class GatewayRuntime:
    state: GatewayState
    enabled: bool = False
    host: str = "127.0.0.1"
    port: int = 0
    bound_port: int | None = None
    pid: int | None = None
    listening: bool = False
    service_manager: str | None = None
    service_installed: bool = False
    service_running: bool = False

    @property
    def probe_host(self) -> str:
        """Host to actually connect to. 0.0.0.0 / :: are bind-only wildcards —
        connecting to them is not the same as connecting to the service."""
        return "127.0.0.1" if self.host in _WILDCARD_HOSTS else self.host

    @property
    def effective_port(self) -> int:
        """The port a client should use: the really-bound one when known
        (gateway.port=0 means the OS picked it), else the configured one."""
        return self.bound_port or self.port


def tcp_listening(host: str, port: int, timeout: float = 0.5) -> bool:
    if not port:
        return False
    target = "127.0.0.1" if host in _WILDCARD_HOSTS else host
    try:
        with socket.create_connection((target, int(port)), timeout=timeout):
            return True
    except Exception:  # noqa: BLE001 - "cannot connect" is the only answer here
        # OSError covers refused/timeout/unresolvable; a corrupt endpoint file
        # can also hand us a non-numeric, out-of-range or wrong-typed port/host,
        # and "not listening" is the right answer for all of them.
        return False


def _as_int(value: Any) -> int | None:
    """Coerce an untrusted port/pid field to a positive int, or None.

    Both sources are outside our control: the endpoint file is written by
    another process, and the YAML is hand-edited. So ``"port": "not-a-port"``
    and ``port: .inf`` alike must degrade to "unknown" here rather than raise.

    ``OverflowError`` matters specifically: ``json.loads`` accepts the
    non-standard ``Infinity`` literal, and ``int(float("inf"))`` raises it. That
    used to escape to the outer guard and return the DISABLED fallback, which
    reports ``enabled=False`` for a gateway the user has enabled — a wrong
    answer is worse than a missing one, so it is contained at the source.
    """
    try:
        number = int(value)
    except (TypeError, ValueError, OverflowError):
        return None
    return number if number > 0 else None


def _read_endpoint(workspace: Path) -> dict | None:
    from echo_agent.cli.workspace import read_runtime_endpoint

    try:
        result = read_runtime_endpoint(workspace)
    except Exception:  # noqa: BLE001 - endpoint file may be absent/corrupt
        return None
    return result if isinstance(result, dict) else None


def _resolve_workspace(config: Any, config_path: str | None, workspace: str | None) -> Path:
    from echo_agent.cli.workspace import resolve_effective_workspace

    try:
        return resolve_effective_workspace(config, config_path, workspace)
    except Exception:  # noqa: BLE001 - fall back to cwd rather than fail the probe
        return Path(".")


def _detect_backend() -> Any:
    """The user-scope service backend, or None on platforms/environments with
    no usable service manager (WSL without systemd, containers, Windows)."""
    from echo_agent.cli.service import detect_backend

    try:
        return detect_backend(system=False)
    except Exception:  # noqa: BLE001 - service probing is advisory only
        return None


def _load_config(config_path: str | None, workspace: str | None) -> Any:
    from echo_agent.config.loader import load_config, resolve_config_file

    cp = config_path
    if cp is None and workspace:
        cp = str(resolve_config_file(search_dir=workspace) or "") or None
    overrides = {"workspace": workspace} if workspace else None
    return load_config(config_path=cp, overrides=overrides)


def probe_gateway(
    config: Any = None,
    config_path: str | None = None,
    workspace: str | None = None,
) -> GatewayRuntime:
    """Probe the live gateway state. Never raises.

    ``config`` may be passed in by callers that already loaded it (status, the
    setup wizard) so a single command does not re-read the same YAML repeatedly.

    The individual sub-probes below already degrade on their own; this outer
    guard makes "never raises" a property of the module rather than of every
    future edit to it. Callers are on a failure path already — a probe that
    throws would replace actionable guidance with a traceback.
    """
    try:
        return _probe_gateway(config, config_path, workspace)
    except Exception:  # noqa: BLE001 - the contract outranks any single sub-probe
        return GatewayRuntime(state=GatewayState.DISABLED)


def _probe_gateway(
    config: Any,
    config_path: str | None,
    workspace: str | None,
) -> GatewayRuntime:
    if config is None:
        try:
            config = _load_config(config_path, workspace)
        except Exception:  # noqa: BLE001 - unreadable config is itself an answer
            return GatewayRuntime(state=GatewayState.DISABLED)

    try:
        gw = config.gateway
        enabled = bool(gw.enabled)
        host = str(gw.host)
        # An unusable configured port must not cost us the rest of the answer:
        # `enabled` is the field callers act on, and reporting it as False for a
        # gateway the user enabled sends them to fix a setting that is already
        # right. 0 is the established "no usable port" value (gateway.port=0
        # already means "ask the endpoint file"), so keep that, not None.
        port = _as_int(gw.port) or 0
    except Exception:  # noqa: BLE001 - a config we cannot read the gateway out of
        return GatewayRuntime(state=GatewayState.DISABLED)

    backend = _detect_backend()
    try:
        manager = getattr(backend, "name", None) if backend is not None else None
        installed = bool(backend.is_installed()) if backend is not None else False
        running = bool(backend.is_running()) if backend is not None else False
    except Exception:  # noqa: BLE001 - a backend that cannot answer is no backend
        backend, manager, installed, running = None, None, False, False

    if not enabled:
        # The gateway component is off. Channels (WeChat/QQ) do not depend on it
        # and keep working — callers say so rather than implying a dead service.
        return GatewayRuntime(
            state=GatewayState.DISABLED, enabled=False, host=host, port=port,
            service_manager=manager, service_installed=installed, service_running=running,
        )

    endpoint = _read_endpoint(_resolve_workspace(config, config_path, workspace))
    if not isinstance(endpoint, dict):
        endpoint = {}
    bound_port = _as_int(endpoint.get("port"))
    pid = _as_int(endpoint.get("pid"))
    effective_host = str(endpoint.get("host") or host)
    listening = bool(tcp_listening(effective_host, bound_port or port))

    if listening:
        state = GatewayState.RUNNING
    elif backend is None:
        state = GatewayState.NO_SERVICE_MANAGER
    elif installed:
        # Covers the case systemd calls "active" while the port is dead: the
        # unit forked fine but bootstrap died (bad API key, port taken). The
        # actionable next step is the log, not another `start`.
        state = GatewayState.SERVICE_INSTALLED_STOPPED
    else:
        state = GatewayState.NOT_INSTALLED

    return GatewayRuntime(
        state=state, enabled=True, host=effective_host, port=port,
        bound_port=bound_port, pid=pid, listening=listening,
        service_manager=manager, service_installed=installed, service_running=running,
    )


def is_wsl() -> bool:
    """True on WSL.

    Lives here because it is the same kind of fact as the rest of this module:
    something about the local environment that guidance must branch on. WSL
    without `systemd=true` in /etc/wsl.conf has systemctl but no running system
    manager, so its advice differs from a plain container's — enabling systemd is
    a real option for WSL users, and the generic message used to hide it.
    """
    import sys

    if sys.platform != "linux":
        return False
    try:
        return "microsoft" in Path("/proc/version").read_text(encoding="utf-8").lower()
    except OSError:
        return False
