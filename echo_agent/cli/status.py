"""Status command — display configuration summary and live runtime state.

Beyond the static config summary this probes the actual runtime: the recorded
gateway endpoint (pid/port), whether that port is really accepting TCP
connections, the background service state, and — when available — the shared
health probes. Supports ``--json`` for machine-readable output and returns a
stable process exit code (0 healthy, non-zero when a problem is detected).
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

from echo_agent.cli.colors import (
    Colors,
    color,
    print_header,
    print_info,
    set_color_override,
)
from echo_agent.config.loader import load_config, resolve_config_file
from echo_agent.config.schema import ProviderConfig

_CHANNEL_NAMES = [
    "cli", "webhook", "cron", "telegram", "discord", "slack",
    "whatsapp", "weixin", "qqbot", "feishu", "dingtalk",
    "email", "wecom", "matrix",
]


def _provider_credential_status(provider: ProviderConfig) -> tuple[str, str]:
    name = provider.name.lower()
    if provider.credential_pool:
        return "credential pool configured", Colors.GREEN
    if provider.api_key:
        return "API key configured", Colors.GREEN
    if name in ("bedrock", "aws"):
        return "uses AWS environment/profile", Colors.CYAN
    return "API key missing", Colors.YELLOW


def _resolve_workspace(config: Any, config_file: Path | None, override: str | None) -> Path:
    """Resolve the effective workspace via the one authoritative rule."""
    from echo_agent.cli.workspace import resolve_effective_workspace

    return resolve_effective_workspace(
        config, str(config_file) if config_file else None, override
    )


def _health_probes(config: Any) -> list[dict] | None:
    """Run the shared health probes (the same ones ``setup doctor`` renders).

    Each probe is a dict with name/status/detail. Returns None when probing
    fails outright, so status still renders its own simpler checks — health is
    advisory and must never break the status report.
    """
    from echo_agent.cli.health import run_health_checks

    try:
        return run_health_checks(config)
    except Exception:  # noqa: BLE001 - health is advisory
        return None


def _enabled_channels(config: Any) -> list[str]:
    enabled = []
    for name in _CHANNEL_NAMES:
        ch_cfg = getattr(config.channels, name, None)
        if ch_cfg is not None and getattr(ch_cfg, "enabled", False):
            enabled.append(name)
    return enabled


def _channel_summary(enabled: list[str]) -> str:
    """Accurate, non-contradictory channel description.

    The old code always claimed "Only CLI channel is active by default" even
    when cli was explicitly disabled. Report what is actually on.
    """
    if not enabled:
        return "No channels enabled — the agent will not receive messages"
    if enabled == ["cli"]:
        return "Only the CLI channel is enabled"
    return f"{len(enabled)} channel(s) enabled: {', '.join(enabled)}"


def _gather_status(config_path: str | Path | None, workspace: str | Path | None) -> dict[str, Any]:
    """Collect the full status snapshot as a plain dict (config + runtime).

    Shared by the text and JSON renderers so both stay in sync.
    """
    config_file = resolve_config_file(config_path=config_path, search_dir=workspace)
    config_file_exists = bool(config_file and config_file.exists())

    overrides = {"workspace": str(workspace)} if workspace else None
    config = load_config(config_path=config_file, overrides=overrides)
    effective_workspace = _resolve_workspace(
        config,
        config_file if config_file_exists else None,
        str(workspace) if workspace else None,
    )

    providers = []
    for p in config.models.providers:
        credential_text, _ = _provider_credential_status(p)
        providers.append({
            "name": p.name or "<unnamed>",
            "models": list(p.models[:3]) if p.models else [],
            "credential": credential_text,
        })

    enabled_channels = _enabled_channels(config)

    from echo_agent.cli.runtime_probe import probe_gateway

    runtime = probe_gateway(
        config=config,
        config_path=str(config_file) if config_file_exists else None,
        workspace=str(workspace) if workspace else None,
    )
    health = _health_probes(config)

    return {
        "config_file": str(config_file) if config_file else None,
        "config_file_exists": config_file_exists,
        "workspace": str(effective_workspace),
        "providers": providers,
        "default_model": config.models.default_model,
        "channels": {"enabled": enabled_channels, "summary": _channel_summary(enabled_channels)},
        "gateway": {
            "enabled": runtime.enabled,
            "host": runtime.host,
            "configured_port": config.gateway.port,
            "running_pid": runtime.pid,
            "running_port": runtime.bound_port,
            "listening": runtime.listening,
        },
        "service": (
            None if runtime.service_manager is None
            else {"installed": runtime.service_installed, "running": runtime.service_running}
        ),
        "health": health,
    }


def _status_exit_code(data: dict[str, Any]) -> int:
    """0 when healthy; non-zero when a real problem is detected.

    Problems: no config file, no providers configured, an enabled gateway that
    isn't actually listening, or any failing shared health probe. A disabled
    gateway that isn't listening is expected and does NOT fail.
    """
    if not data["config_file_exists"]:
        return 1
    if not data["providers"]:
        return 1
    gw = data["gateway"]
    if gw["enabled"] and not gw["listening"]:
        return 1
    health = data["health"]
    if health:
        for probe in health:
            if str(probe.get("status", "")).lower() in ("fail", "error", "unhealthy", "down"):
                return 1
    return 0


def _render_text(data: dict[str, Any]) -> None:
    print_header("Echo Agent Status")

    if data["config_file"] and data["config_file_exists"]:
        print(f"  Config file:  {color(data['config_file'], Colors.CYAN)}")
    elif data["config_file"]:
        print(f"  Config file:  {color(data['config_file'] + ' (not found)', Colors.YELLOW)}")
    else:
        print(f"  Config file:  {color('not found', Colors.YELLOW)}")
    print(f"  Workspace:    {color(data['workspace'], Colors.CYAN)}")
    print()

    # Providers
    print_header("LLM Providers")
    if data["providers"]:
        for p in data["providers"]:
            models = ", ".join(p["models"]) if p["models"] else "—"
            clr = Colors.YELLOW if "missing" in p["credential"] else Colors.GREEN
            print(
                f"  {color(p['name'], Colors.GREEN)}"
                f"  models: {models}"
                f"  {color(p['credential'], clr)}"
            )
    else:
        print_info("No providers configured")
    print(f"  Default model: {color(data['default_model'], Colors.CYAN)}")
    print()

    # Channels
    print_header("Channels")
    enabled = set(data["channels"]["enabled"])
    for name in _CHANNEL_NAMES:
        if name in enabled:
            print(f"  {color('●', Colors.GREEN)} {name}")
    print_info(data["channels"]["summary"])
    print()

    # Gateway (config + live runtime)
    print_header("Gateway")
    gw = data["gateway"]
    if gw["enabled"]:
        print(f"  {color('●', Colors.GREEN)} Enabled on {gw['host']}:{gw['configured_port']}")
    else:
        print(f"  {color('○', Colors.DIM)} Disabled in config")
    if gw["running_pid"] or gw["running_port"]:
        pid = gw["running_pid"] if gw["running_pid"] is not None else "unknown"
        port = gw["running_port"] if gw["running_port"] is not None else "unknown"
        print(f"  Recorded endpoint: pid {pid}, port {port}")
    else:
        print_info("Recorded endpoint: 未运行/未知 (no endpoint recorded)")
    if gw["listening"]:
        live_port = gw["running_port"] or gw["configured_port"]
        print(f"  {color('●', Colors.GREEN)} Port {live_port} is accepting connections")
    else:
        print(f"  {color('○', Colors.DIM)} Port not listening")
    if data["service"] is not None:
        svc = data["service"]
        if svc["running"]:
            state = "running"
        elif svc["installed"]:
            state = "installed, stopped"
        else:
            state = "not installed"
        print(f"  Background service: {state}")
    print()

    # Health probes (optional shared module)
    if data["health"]:
        print_header("Health")
        for probe in data["health"]:
            status = str(probe.get("status", "")).lower()
            if status in ("ok", "pass", "healthy", "up"):
                mark = color("●", Colors.GREEN)
            elif status in ("fail", "error", "unhealthy", "down"):
                mark = color("✗", Colors.RED)
            else:  # warn / unknown
                mark = color("!", Colors.YELLOW)
            detail = probe.get("detail")
            line = f"  {mark} {probe.get('name', '?')}: {probe.get('status', '?')}"
            if detail:
                line += f" — {detail}"
            print(line)
        print()


def show_status(
    config_path: str | Path | None = None,
    workspace: str | Path | None = None,
    as_json: bool = False,
) -> int:
    """Render the status report and return a process exit code.

    ``as_json`` emits a structured JSON document with color forced off.
    Exit code: 0 healthy, non-zero when a problem is detected.
    """
    if as_json:
        set_color_override(False)
        try:
            data = _gather_status(config_path, workspace)
        finally:
            set_color_override(None)
        print(json.dumps(data, ensure_ascii=False, indent=2))
        return _status_exit_code(data)

    data = _gather_status(config_path, workspace)
    _render_text(data)
    return _status_exit_code(data)


