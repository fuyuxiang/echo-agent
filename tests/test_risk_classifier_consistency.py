# tests/test_risk_classifier_consistency.py
"""The static risk map and each tool's declared risk_level must not disagree.

Both describe one fact — "how dangerous is calling this tool" — and the gate
reads them through classify_risk(). While the map won unconditionally, a
disagreement was invisible and permanent: DelegateTool/SpawnTool declared
risk_level="exec" and were gated as WRITE anyway, which made delegating a
command a cheaper route to EXEC than running one. Nothing failed; the two
sources simply drifted.

These tests are the fence. The first pins the specific escalation that was
exploitable, the second is the general property so a future tool cannot
reintroduce the same class of hole by adding a map entry that is weaker than
what the tool itself claims.
"""
from __future__ import annotations

import pytest

from echo_agent.security.risk_classifier import (
    _SEVERITY,
    _TOOL_RISK_MAP,
    RiskLevel,
    classify_risk,
)


# Tools whose dispatch hands a goal to an agent that then calls tools of its own.
# Their own risk must be at least EXEC: the worker can reach exec, and the
# dispatch is the last point where the *caller's* authority is still known.
_WORKER_DISPATCH_TOOLS = ("delegate_task", "spawn_task")


@pytest.mark.parametrize("tool_name", _WORKER_DISPATCH_TOOLS)
def test_worker_dispatch_is_exec_tier(tool_name: str) -> None:
    """A worker dispatcher is never gated more cheaply than what it can reach."""
    risk = classify_risk(tool_name)
    assert _SEVERITY[risk] >= _SEVERITY[RiskLevel.EXEC], (
        f"{tool_name} classified as {risk.value}; a worker it spawns can call "
        "exec, so dispatching one must not be cheaper than running a command"
    )


@pytest.mark.parametrize("tool_name", _WORKER_DISPATCH_TOOLS)
def test_worker_dispatch_declares_exec(tool_name: str) -> None:
    """The tool classes themselves declare it too, so either source alone holds."""
    from echo_agent.agent.tools.delegate import DelegateTool, SpawnTool

    tool_cls = {"delegate_task": DelegateTool, "spawn_task": SpawnTool}[tool_name]
    declared = RiskLevel(tool_cls.risk_level)
    assert _SEVERITY[declared] >= _SEVERITY[RiskLevel.EXEC]


def test_static_map_never_weaker_than_declaration() -> None:
    """No map entry may under-gate a tool relative to its own declaration.

    Walks the real tool classes rather than a hand-kept list: a new tool that
    declares a high risk_level and gets a low map entry is exactly the drift
    this test exists to catch, and it would not be in any list written today.
    """
    import inspect

    from echo_agent.tools.base import Tool

    checked = 0
    for module_name in (
        "echo_agent.agent.tools.delegate",
        "echo_agent.agent.tools.exec",
        "echo_agent.agent.tools.web",
        "echo_agent.agent.tools.message",
        "echo_agent.agent.tools.clarify",
    ):
        try:
            module = __import__(module_name, fromlist=["*"])
        except ImportError:
            continue
        for _, obj in inspect.getmembers(module, inspect.isclass):
            if not issubclass(obj, Tool) or obj is Tool:
                continue
            name = getattr(obj, "name", "")
            declared_raw = getattr(obj, "risk_level", "")
            if not name or not declared_raw or name not in _TOOL_RISK_MAP:
                continue
            try:
                declared = RiskLevel(declared_raw)
            except ValueError:
                continue
            checked += 1
            effective = classify_risk(name, tool_risk_level=declared_raw)
            assert _SEVERITY[effective] >= _SEVERITY[declared], (
                f"{name}: static map says {_TOOL_RISK_MAP[name].value} but the "
                f"tool declares {declared.value}; classify_risk returned "
                f"{effective.value}, which under-gates the tool"
            )
    assert checked, "no tool with both a map entry and a declaration was found"


def test_stricter_of_the_two_sources_wins() -> None:
    """The resolution rule itself, independent of any current tool."""
    # Declaration stricter than the map → declaration wins.
    assert classify_risk("write_file", tool_risk_level="dangerous") is RiskLevel.DANGEROUS
    # Map stricter than the declaration → map wins.
    assert classify_risk("exec", tool_risk_level="read_only") is RiskLevel.EXEC
    # Unknown tool with a declaration → the declaration.
    assert classify_risk("some_mcp_tool", tool_risk_level="exec") is RiskLevel.EXEC
    # Unknown tool, no declaration → the WRITE default.
    assert classify_risk("some_mcp_tool") is RiskLevel.WRITE
    # Garbage declaration is ignored, not crashed on.
    assert classify_risk("exec", tool_risk_level="not-a-level") is RiskLevel.EXEC
    assert classify_risk("some_mcp_tool", tool_risk_level="nonsense") is RiskLevel.WRITE
