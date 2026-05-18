"""Test plugin fixture for integration tests."""

from __future__ import annotations

from typing import Any

from echo_agent.agent.tools.base import Tool, ToolExecutionContext, ToolResult
from echo_agent.plugins.hooks import HookResult


call_log: list[dict[str, Any]] = []


class TestEchoTool(Tool):
    name = "test_echo"
    description = "Echoes back the input text. For testing only."
    parameters = {
        "type": "object",
        "properties": {
            "text": {"type": "string", "description": "Text to echo back."},
        },
        "required": ["text"],
    }
    risk_level = "read_only"

    async def execute(self, params: dict[str, Any], ctx: ToolExecutionContext | None = None) -> ToolResult:
        return ToolResult(success=True, output=f"echo: {params['text']}")

    def execution_mode(self, params: dict[str, Any]) -> str:
        return "read_only"


async def _on_pre_tool_call(tool_name: str, params: dict[str, Any], ctx: Any) -> HookResult | None:
    call_log.append({"hook": "pre_tool_call", "tool": tool_name, "params": params})
    return None


async def _on_post_tool_call(result: Any, tool_name: str, params: dict[str, Any], ctx: Any) -> HookResult | None:
    call_log.append({"hook": "post_tool_call", "tool": tool_name, "success": getattr(result, "success", None)})
    return None


async def activate(ctx) -> None:
    ctx.register_tool(TestEchoTool())
    ctx.register_hook("pre_tool_call", _on_pre_tool_call)
    ctx.register_hook("post_tool_call", _on_post_tool_call)
    call_log.append({"hook": "activated", "plugin": ctx.plugin_name})


async def deactivate(ctx) -> None:
    call_log.append({"hook": "deactivated", "plugin": ctx.plugin_name})


plugin = {
    "activate": activate,
    "deactivate": deactivate,
}
