"""Architecture guards for uniform subprocess ownership and cleanup."""

from __future__ import annotations

import ast
from pathlib import Path
from unittest.mock import AsyncMock

import pytest

from echo_agent.media import silk


_PACKAGE_ROOT = Path(__file__).parents[1] / "echo_agent"
_RAW_ASYNC_SPAWN = {"create_subprocess_exec", "create_subprocess_shell"}
_RAW_SUBPROCESS_CALLS = {"Popen", "run"}


@pytest.mark.asyncio
async def test_ffmpeg_routes_communication_through_owned_helper(monkeypatch):
    proc = AsyncMock()
    communicate = AsyncMock(side_effect=OSError("pipe broke"))
    monkeypatch.setattr(silk, "_resolve_ffmpeg", lambda: "/usr/bin/ffmpeg")
    monkeypatch.setattr(silk, "spawn_exec", AsyncMock(return_value=proc))
    monkeypatch.setattr(silk, "communicate_owned", communicate)

    with pytest.raises(OSError, match="pipe broke"):
        await silk._ffmpeg_to_pcm("input.wav", "output.pcm")

    communicate.assert_awaited_once_with(proc, timeout=silk._FFMPEG_TIMEOUT)


def test_production_subprocesses_use_proc_lifecycle_entrypoints() -> None:
    violations: list[str] = []
    lifecycle_path = _PACKAGE_ROOT / "agent" / "proc_lifecycle.py"
    for path in sorted(_PACKAGE_ROOT.rglob("*.py")):
        if path == lifecycle_path:
            continue
        tree = ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
        blocked_imports: set[str] = set()
        module_aliases: dict[str, set[str]] = {"asyncio": set(), "subprocess": set()}
        for imported in (node for node in ast.walk(tree) if isinstance(node, ast.Import)):
            for alias in imported.names:
                if alias.name in module_aliases:
                    module_aliases[alias.name].add(alias.asname or alias.name)
        for imported in (node for node in ast.walk(tree) if isinstance(node, ast.ImportFrom)):
            if imported.module not in {"asyncio", "subprocess"}:
                continue
            for alias in imported.names:
                if alias.name in _RAW_ASYNC_SPAWN or alias.name in _RAW_SUBPROCESS_CALLS:
                    blocked_imports.add(alias.asname or alias.name)
        for node in (candidate for candidate in ast.walk(tree) if isinstance(candidate, ast.Call)):
            func = node.func
            raw_name = ""
            if isinstance(func, ast.Attribute):
                if isinstance(func.value, ast.Name):
                    owner = func.value.id
                    if (
                        owner in module_aliases["asyncio"]
                        and func.attr in _RAW_ASYNC_SPAWN
                    ) or (
                        owner in module_aliases["subprocess"]
                        and func.attr in _RAW_SUBPROCESS_CALLS
                    ):
                        raw_name = func.attr
            elif isinstance(func, ast.Name) and func.id in blocked_imports:
                raw_name = func.id
            if raw_name in _RAW_ASYNC_SPAWN or raw_name in _RAW_SUBPROCESS_CALLS:
                violations.append(
                    f"{path.relative_to(_PACKAGE_ROOT.parent)}:{node.lineno}:{raw_name}"
                )
            if isinstance(func, ast.Attribute) and func.attr == "communicate":
                violations.append(
                    f"{path.relative_to(_PACKAGE_ROOT.parent)}:{node.lineno}:communicate"
                )

    assert violations == [], (
        "production subprocesses must use agent.proc_lifecycle spawn/run helpers "
        "and owned completion (long-lived owners use stream/wait APIs): "
        + ", ".join(violations)
    )


def test_long_lived_process_owners_do_not_use_one_shot_communicate_helper() -> None:
    """ProcessTool and MCP stdio retain ownership until stop()/close()."""
    long_lived = [
        _PACKAGE_ROOT / "agent" / "tools" / "process.py",
        _PACKAGE_ROOT / "mcp" / "transport.py",
    ]
    violations = [
        str(path.relative_to(_PACKAGE_ROOT.parent))
        for path in long_lived
        if "communicate_owned" in path.read_text(encoding="utf-8")
    ]
    assert violations == []
