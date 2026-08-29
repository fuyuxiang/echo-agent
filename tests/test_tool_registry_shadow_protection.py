"""Tool names and aliases cannot be silently shadowed."""

from __future__ import annotations

import pytest

from echo_agent.agent.tools.registry import ToolRegistry
from echo_agent.tools import Tool, ToolResult


class _NamedTool(Tool):
    description = "test tool"
    parameters = {"type": "object", "properties": {}}

    def __init__(self, name: str, marker: str = "ok") -> None:
        self.name = name
        self._marker = marker

    async def execute(self, params, ctx=None):
        return ToolResult(output=self._marker)


def test_duplicate_name_is_rejected_and_original_remains() -> None:
    registry = ToolRegistry()
    original = _NamedTool("exec", "builtin")
    registry.register(original)

    with pytest.raises(ValueError, match="already registered"):
        registry.register(_NamedTool("exec", "plugin"))

    assert registry.get("exec") is original
    assert registry.get("bash") is original


@pytest.mark.parametrize("alias", ["bash", "shell", "run_code", "code"])
def test_alias_names_are_reserved_even_before_target_registration(alias: str) -> None:
    registry = ToolRegistry()
    with pytest.raises(ValueError, match="reserved as an alias"):
        registry.register(_NamedTool(alias))
    assert alias not in registry.tool_names


def test_exact_object_reregistration_is_idempotent() -> None:
    registry = ToolRegistry()
    tool = _NamedTool("custom")
    registry.register(tool)
    registry.register(tool)
    assert registry.tool_names == ["custom"]


def test_trusted_explicit_replacement_is_visible() -> None:
    registry = ToolRegistry()
    original = _NamedTool("custom", "old")
    replacement = _NamedTool("custom", "new")
    registry.register(original)
    registry.register(replacement, replace=True)
    assert registry.get("custom") is replacement


@pytest.mark.parametrize("invalid", ["", "   ", "bad name", "bad\nname", 123])
def test_invalid_tool_names_fail_before_mutating_registry(invalid) -> None:
    registry = ToolRegistry()
    tool = _NamedTool("valid")
    tool.name = invalid
    with pytest.raises(ValueError, match="Tool name must be"):
        registry.register(tool)
    assert registry.tool_names == []
