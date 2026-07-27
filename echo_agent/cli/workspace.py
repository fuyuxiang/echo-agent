"""Authoritative workspace resolution and gateway runtime-endpoint helpers.

Historically each entrypoint resolved a relative ``workspace: ./data`` on its
own: ``app.bootstrap`` resolved it against the *config file's* directory, while
``plugin``/``checkpoint``/``migrate`` resolved it against the *shell cwd*. The
same config then landed in different directories depending on which command ran.

:func:`resolve_effective_workspace` is the single rule every entrypoint must
call. The runtime-endpoint functions are the shared contract between the
gateway (writer) and ``attach``/``service status`` (readers): the actually bound
port only ever surfaced on stdout, so anything that didn't parse the log could
not learn the real port when ``gateway.port=0``.
"""

from __future__ import annotations

import json
import os
from pathlib import Path
from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    from echo_agent.config.schema import Config

_ENDPOINT_DIR = ".echo-agent"
_ENDPOINT_FILE = "gateway.json"


def resolve_effective_workspace(
    config: "Config | Any",
    config_file: str | None,
    override: str | None,
) -> Path:
    """Resolve the effective workspace directory — the one authoritative rule.

    - ``override`` set (an explicit ``-w``): resolve it relative to the shell
      cwd (absolute stays as-is), matching "the workspace the user typed".
    - otherwise: a relative ``config.workspace`` is resolved against the config
      file's directory (falling back to cwd when no config file was found), so a
      ``workspace: ./data`` sitting next to ``echo-agent.yaml`` always lands next
      to that file regardless of where the command was launched.
    - absolute values are used verbatim; ``~`` is expanded in every case.
    """
    raw = override if override is not None else config.workspace
    path = Path(raw).expanduser()
    if path.is_absolute():
        return path.resolve()
    if override is not None:
        base = Path.cwd()
    else:
        base = Path(config_file).expanduser().parent if config_file else Path.cwd()
    return (base / path).resolve()


def load_config_and_workspace(
    config_path: str | None, workspace: str | None
) -> tuple["Config | Any", Path]:
    """Load the config and resolve its effective workspace in one call.

    The shared entrypoint for subcommands (``plugin``/``checkpoint``/
    ``migrate``/``cost``) that need both: it pins config-file discovery to
    ``search_dir=workspace`` and then applies :func:`resolve_effective_workspace`,
    so every command lands on the exact same directory ``app.bootstrap`` would
    for a given ``workspace: ./data``. Previously each command reimplemented
    this (or imported another command's private helper) and they drifted.
    """
    from echo_agent.config.loader import load_config, resolve_config_file

    config_file = resolve_config_file(config_path, search_dir=workspace)
    overrides = {"workspace": workspace} if workspace else None
    config = load_config(config_path=config_file, overrides=overrides)
    ws = resolve_effective_workspace(
        config, str(config_file) if config_file else None, workspace
    )
    return config, ws


def endpoint_path(workspace: Path) -> Path:
    """Path of the gateway runtime-endpoint file inside ``workspace``."""
    return workspace / _ENDPOINT_DIR / _ENDPOINT_FILE


def write_runtime_endpoint(
    workspace: Path, *, host: str, port: int, pid: int, ws_path: str
) -> None:
    """Persist the gateway's actually bound endpoint for readers to discover.

    Written once the server has bound and knows its real ``port`` (which differs
    from the configured one when ``gateway.port=0``). Creates the ``.echo-agent``
    directory if needed and writes atomically via a temp file + rename so a
    concurrent reader never sees a half-written document.
    """
    path = endpoint_path(workspace)
    path.parent.mkdir(parents=True, exist_ok=True)
    payload = {"host": host, "port": port, "pid": pid, "ws_path": ws_path}
    tmp = path.with_suffix(path.suffix + ".tmp")
    tmp.write_text(json.dumps(payload), encoding="utf-8")
    os.replace(tmp, path)


def read_runtime_endpoint(workspace: Path) -> dict | None:
    """Read the gateway runtime endpoint, or ``None`` if missing/corrupt."""
    path = endpoint_path(workspace)
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return None
    return data if isinstance(data, dict) else None


def clear_runtime_endpoint(workspace: Path) -> None:
    """Best-effort removal of the runtime-endpoint file on shutdown."""
    try:
        endpoint_path(workspace).unlink()
    except OSError:
        pass
