"""Structured health probes for the setup ``doctor`` (and future ``status``).

Every probe is a small, side-effect-light function that inspects the resolved
config and the local environment, returning a uniform ``dict`` so the CLI can
render it and a future ``status`` command can reuse the exact same logic:

    {"name": str, "status": "ok" | "warn" | "fail", "detail": str}

Design constraints:
  - Pure w.r.t. config: probes never mutate the passed-in config.
  - No outbound network calls. Provider credentials are checked for presence
    only ("connectivity unverified"); the gateway probe does a local TCP
    connect to see whether a listener is up, nothing more.
  - Accepts a plain ``dict`` (what the setup wizard carries), a pydantic
    ``Config`` (``.model_dump``-able), or ``None`` (loads the real config).
"""

from __future__ import annotations

import os
import shutil
import socket
from pathlib import Path
from typing import Any

from echo_agent.cli.i18n import get_locale

OK = "ok"
WARN = "warn"
FAIL = "fail"


# ── i18n ──────────────────────────────────────────────────────────────────────
# Health strings live here (keyed by the active locale) rather than in the
# shared i18n bundles: the probes are self-contained and reused by a future
# ``status`` command, so co-locating the copy keeps them dependency-light.
# Locale still flows through ``get_locale()`` so this honours ``--lang``.

_STRINGS: dict[str, dict[str, str]] = {
    "zh": {
        "provider_name": "Provider 凭证",
        "provider_none": "未配置 Provider（运行 'echo-agent setup model'）",
        "provider_item": "Provider 凭证：{name}",
        "provider_key_present": "已配置 API key（未验证连通性）",
        "provider_no_key_ok": "本地/桩 Provider，无需 API key",
        "provider_key_missing": "缺少 API key",
        "gateway_name": "Gateway",
        "gateway_off": "未启用",
        "gateway_no_port": "已启用但端口为 0，无法探测",
        "gateway_listening": "正在监听 {host}:{port}",
        "gateway_not_listening": "已启用但 {host}:{port} 未在监听（服务可能未启动）",
        "storage_name": "数据库/存储",
        "storage_ok": "数据目录可写：{path}",
        "storage_fail": "数据目录不存在或不可写：{path}",
        "workspace_name": "工作区",
        "workspace_ok": "工作区可读写：{path}",
        "workspace_missing": "工作区尚未创建（首次运行时自动创建）：{path}",
        "workspace_not_dir": "工作区路径存在但不是目录：{path}",
        "workspace_no_access": "工作区不可读写：{path}",
        "code_exec_name": "容器/解释器",
        "code_exec_ok": "{binary} 可用：{path}",
        "code_exec_missing": "code_exec 已启用但 PATH 中找不到 {binary}",
        "mcp_name": "MCP",
        "mcp_servers": "已配置 {n} 个 MCP 服务器",
        "mcp_no_servers": "MCP 已启用但未配置任何服务器",
    },
    "en": {
        "provider_name": "provider credentials",
        "provider_none": "no provider configured (run 'echo-agent setup model')",
        "provider_item": "provider credentials: {name}",
        "provider_key_present": "API key present (connectivity unverified)",
        "provider_no_key_ok": "local/stub provider, no API key required",
        "provider_key_missing": "API key missing",
        "gateway_name": "gateway",
        "gateway_off": "disabled",
        "gateway_no_port": "enabled but port is 0, cannot probe",
        "gateway_listening": "listening on {host}:{port}",
        "gateway_not_listening": "enabled but nothing listening on {host}:{port} (service may be down)",
        "storage_name": "database/storage",
        "storage_ok": "data directory writable: {path}",
        "storage_fail": "data directory missing or not writable: {path}",
        "workspace_name": "workspace",
        "workspace_ok": "workspace readable/writable: {path}",
        "workspace_missing": "workspace not created yet (auto-created on first run): {path}",
        "workspace_not_dir": "workspace path exists but is not a directory: {path}",
        "workspace_no_access": "workspace not readable/writable: {path}",
        "code_exec_name": "container/interpreter",
        "code_exec_ok": "{binary} available: {path}",
        "code_exec_missing": "code_exec enabled but {binary} not found on PATH",
        "mcp_name": "MCP",
        "mcp_servers": "{n} MCP server(s) configured",
        "mcp_no_servers": "MCP enabled but no servers configured",
    },
}


def t(key: str, **kwargs: Any) -> str:
    """Translate a ``doctor.<name>`` key via the active locale.

    Mirrors the shape of ``i18n.t`` (dotted key, ``str.format`` kwargs, English
    fallback) but resolves against this module's self-contained table.
    """
    name = key.split(".", 1)[-1]
    locale = get_locale() if get_locale() in _STRINGS else "en"
    value = _STRINGS[locale].get(name) or _STRINGS["en"].get(name) or key
    if kwargs:
        try:
            return value.format(**kwargs)
        except (KeyError, IndexError):
            return value
    return value


# ── config access helpers (tolerant of snake_case / camelCase) ────────────────

def _as_dict(config: Any) -> dict:
    """Normalise ``config`` into a plain dict.

    ``None`` loads the real config best-effort; a pydantic model is dumped;
    a dict is returned as-is.
    """
    if config is None:
        try:
            from echo_agent.config.loader import load_config

            return load_config().model_dump(by_alias=False)
        except Exception:
            return {}
    if isinstance(config, dict):
        return config
    dump = getattr(config, "model_dump", None)
    if callable(dump):
        try:
            return dump(by_alias=False)
        except Exception:
            return {}
    return {}


def _get(section: Any, *names: str, default: Any = None) -> Any:
    """Return the first present key among ``names`` from a dict section."""
    if not isinstance(section, dict):
        return default
    for name in names:
        if name in section and section[name] is not None:
            return section[name]
    return default


def _sub(config: dict, key: str) -> dict:
    value = config.get(key)
    return value if isinstance(value, dict) else {}


def _resolve_workspace(config: dict) -> Path:
    """Resolve the workspace dir, anchoring relative paths at cwd.

    Mirrors ``setup._resolve_workspace`` so probes look where the runtime
    actually reads/writes; kept local to avoid a setup ↔ health import cycle.
    """
    workspace_raw = config.get("workspace") or "~/.echo-agent"
    ws = Path(str(workspace_raw)).expanduser()
    if not ws.is_absolute():
        ws = (Path.cwd() / ws).resolve()
    return ws


def _dir_writable(path: Path) -> bool:
    """True if ``path`` exists and is writable, walking up to the nearest
    existing ancestor when the leaf doesn't exist yet (so a not-yet-created
    data dir under a writable parent still counts as OK)."""
    probe = path
    while not probe.exists():
        parent = probe.parent
        if parent == probe:
            return False
        probe = parent
    return probe.is_dir() and os.access(probe, os.W_OK)


# ── individual probes ─────────────────────────────────────────────────────────

def check_providers(config: dict) -> list[dict]:
    """Presence-only check of API keys for each configured provider.

    Never opens a socket: connectivity is explicitly left unverified so the
    doctor stays fast and side-effect free.
    """
    providers = _get(_sub(config, "models"), "providers", default=[]) or []
    named = [p for p in providers if isinstance(p, dict) and p.get("name")]
    if not named:
        return [{"name": t("doctor.provider_name"), "status": FAIL,
                 "detail": t("doctor.provider_none")}]

    results: list[dict] = []
    for p in named:
        name = p.get("name", "?")
        has_key = bool(_get(p, "api_key", "apiKey", default=""))
        # Local/stub providers legitimately need no key.
        if has_key:
            results.append({"name": t("doctor.provider_item", name=name), "status": OK,
                            "detail": t("doctor.provider_key_present")})
        elif name.lower() in ("stub", "local", "ollama"):
            results.append({"name": t("doctor.provider_item", name=name), "status": OK,
                            "detail": t("doctor.provider_no_key_ok")})
        else:
            results.append({"name": t("doctor.provider_item", name=name), "status": WARN,
                            "detail": t("doctor.provider_key_missing")})
    return results


def check_gateway(config: dict) -> dict:
    """If the gateway is enabled, TCP-probe its port to see if a listener is up."""
    gw = _sub(config, "gateway")
    if not gw.get("enabled"):
        return {"name": t("doctor.gateway_name"), "status": WARN,
                "detail": t("doctor.gateway_off")}
    host = str(_get(gw, "host", default="127.0.0.1"))
    port = int(_get(gw, "port", default=0) or 0)
    connect_host = "127.0.0.1" if host in ("0.0.0.0", "", "::") else host
    if port <= 0:
        return {"name": t("doctor.gateway_name"), "status": WARN,
                "detail": t("doctor.gateway_no_port")}
    try:
        with socket.create_connection((connect_host, port), timeout=0.5):
            return {"name": t("doctor.gateway_name"), "status": OK,
                    "detail": t("doctor.gateway_listening", host=connect_host, port=port)}
    except OSError:
        return {"name": t("doctor.gateway_name"), "status": WARN,
                "detail": t("doctor.gateway_not_listening", host=connect_host, port=port)}


def check_storage(config: dict) -> dict:
    """Check the SQLite db path's directory exists and is writable."""
    ws = _resolve_workspace(config)
    db_rel = _get(_sub(config, "storage"), "database_path", "databasePath",
                  default="data/echo_agent.db")
    db_path = Path(str(db_rel))
    if not db_path.is_absolute():
        db_path = ws / db_path
    db_dir = db_path.parent
    if _dir_writable(db_dir):
        return {"name": t("doctor.storage_name"), "status": OK,
                "detail": t("doctor.storage_ok", path=str(db_dir))}
    return {"name": t("doctor.storage_name"), "status": FAIL,
            "detail": t("doctor.storage_fail", path=str(db_dir))}


def check_workspace(config: dict) -> dict:
    """Check the workspace directory exists (or can be created) and is writable."""
    ws = _resolve_workspace(config)
    if ws.exists() and not ws.is_dir():
        return {"name": t("doctor.workspace_name"), "status": FAIL,
                "detail": t("doctor.workspace_not_dir", path=str(ws))}
    if ws.exists():
        readable = os.access(ws, os.R_OK)
        writable = os.access(ws, os.W_OK)
        if readable and writable:
            return {"name": t("doctor.workspace_name"), "status": OK,
                    "detail": t("doctor.workspace_ok", path=str(ws))}
        return {"name": t("doctor.workspace_name"), "status": FAIL,
                "detail": t("doctor.workspace_no_access", path=str(ws))}
    # Not created yet — OK as long as the nearest existing parent is writable.
    if _dir_writable(ws):
        return {"name": t("doctor.workspace_name"), "status": WARN,
                "detail": t("doctor.workspace_missing", path=str(ws))}
    return {"name": t("doctor.workspace_name"), "status": FAIL,
            "detail": t("doctor.workspace_no_access", path=str(ws))}


def check_code_exec(config: dict) -> dict | None:
    """If code execution is enabled, verify the required interpreter/runtime
    is discoverable on PATH. Returns ``None`` when code_exec is disabled."""
    tools = _sub(config, "tools")
    code_exec = _get(tools, "code_exec", "codeExec", default={}) or {}
    if not (isinstance(code_exec, dict) and code_exec.get("enabled", False)):
        return None
    executor = _get(_sub(config, "execution"), "default_executor", "defaultExecutor",
                    default="sandbox")
    binary = "docker" if executor in ("container", "sandbox") else "python"
    found = shutil.which(binary) or (shutil.which("python3") if binary == "python" else None)
    if found:
        return {"name": t("doctor.code_exec_name"), "status": OK,
                "detail": t("doctor.code_exec_ok", binary=binary, path=found)}
    return {"name": t("doctor.code_exec_name"), "status": WARN,
            "detail": t("doctor.code_exec_missing", binary=binary)}


def check_mcp(config: dict) -> dict | None:
    """If MCP is enabled, report how many servers are configured (no connect).
    Returns ``None`` when MCP is not enabled."""
    tools = _sub(config, "tools")
    mcp = _get(tools, "mcp", default={}) or {}
    servers = _get(tools, "mcp_servers", "mcpServers", default={}) or {}
    enabled = (isinstance(mcp, dict) and mcp.get("enabled")) or bool(servers)
    if not enabled:
        return None
    n = len(servers) if isinstance(servers, (dict, list)) else 0
    if n:
        return {"name": t("doctor.mcp_name"), "status": OK,
                "detail": t("doctor.mcp_servers", n=n)}
    return {"name": t("doctor.mcp_name"), "status": WARN,
            "detail": t("doctor.mcp_no_servers")}


# ── aggregate ─────────────────────────────────────────────────────────────────

def run_health_checks(config: Any = None) -> list[dict]:
    """Run every probe and return a flat ``list[dict]`` of results.

    Pure and re-runnable: accepts a dict, a pydantic ``Config``, or ``None``
    (loads the real config). Safe to call from both ``setup doctor`` and a
    future ``status`` command.
    """
    cfg = _as_dict(config)
    results: list[dict] = []
    results.extend(check_providers(cfg))
    results.append(check_gateway(cfg))
    results.append(check_storage(cfg))
    results.append(check_workspace(cfg))
    for maybe in (check_code_exec(cfg), check_mcp(cfg)):
        if maybe is not None:
            results.append(maybe)
    return results
