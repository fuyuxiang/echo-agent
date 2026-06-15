"""MCP tool adapter — wraps MCP tools as echo-agent Tool instances."""

from __future__ import annotations

import re
from typing import Any

from loguru import logger

from echo_agent.tools.base import Tool, ToolExecutionContext, ToolResult
from echo_agent.mcp.client import MCPClient
from echo_agent.security.risk_classifier import RiskLevel


def _sanitize_name(server: str, tool: str) -> str:
    raw = f"mcp_{server}_{tool}"
    return re.sub(r"[^a-zA-Z0-9_]", "_", raw)


def _convert_mcp_schema(mcp_tool: dict[str, Any]) -> dict[str, Any]:
    schema = mcp_tool.get("inputSchema", {})
    if not schema:
        schema = {"type": "object", "properties": {}}
    return schema


class MCPToolAdapter(Tool):

    timeout_seconds = 120

    def __init__(self, server_name: str, mcp_tool: dict[str, Any], client: MCPClient):
        self._server_name = server_name
        self._mcp_tool_name = mcp_tool.get("name", "")
        self._client = client

        self.name = _sanitize_name(server_name, self._mcp_tool_name)
        self.description = mcp_tool.get("description", f"MCP tool from {server_name}")
        self.parameters = _convert_mcp_schema(mcp_tool)
        self.risk_level = self._classify_risk(mcp_tool).value

    @staticmethod
    def _classify_risk(mcp_tool: dict[str, Any]) -> RiskLevel:
        """按 MCP annotations 保守分级：降级受限、升级顺从。

        destructiveHint → EXEC；readOnlyHint 且非 destructive → READ_ONLY；
        非 dict / 无 hint / 字段矛盾 / 字段非严格 True → WRITE（绝不因外部输入放松）。
        """
        annotations = mcp_tool.get("annotations")
        if not isinstance(annotations, dict):
            return RiskLevel.WRITE

        destructive = annotations.get("destructiveHint") is True
        read_only = annotations.get("readOnlyHint") is True

        if destructive:
            return RiskLevel.EXEC
        # 走到这里 destructive 必为 False（上面已返回）
        if read_only:
            return RiskLevel.READ_ONLY
        return RiskLevel.WRITE

    async def execute(self, params: dict[str, Any], ctx: ToolExecutionContext | None = None) -> ToolResult:
        try:
            resp = await self._client.call_tool(self._mcp_tool_name, params, timeout=self.timeout_seconds)
        except TimeoutError as e:
            return ToolResult(success=False, error=str(e))
        except Exception as e:
            logger.error("MCP tool '{}' call failed: {}", self.name, e)
            return ToolResult(success=False, error=f"MCP call failed: {e}")

        content_parts = resp.get("content", [])
        is_error = resp.get("isError", False)

        text_parts: list[str] = []
        for part in content_parts:
            if isinstance(part, dict):
                if part.get("type") == "text":
                    text_parts.append(part.get("text", ""))
                elif part.get("type") == "image":
                    text_parts.append(f"[image: {part.get('mimeType', 'unknown')}]")
                elif part.get("type") == "resource":
                    res = part.get("resource", {})
                    text_parts.append(f"[resource: {res.get('uri', '')}]\n{res.get('text', '')}")
            elif isinstance(part, str):
                text_parts.append(part)

        output = "\n".join(text_parts)
        return ToolResult(
            success=not is_error,
            output=output if not is_error else "",
            error=output if is_error else "",
            metadata={"mcp_server": self._server_name, "mcp_tool": self._mcp_tool_name},
        )

    def execution_mode(self, params: dict[str, Any]) -> str:
        return "read_only" if self.risk_level == RiskLevel.READ_ONLY.value else "side_effect"
