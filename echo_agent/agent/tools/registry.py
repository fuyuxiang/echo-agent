"""Tool registry — dynamic registration, permission checks, execution with retry/timeout/logging."""

from __future__ import annotations

import asyncio
import uuid
from datetime import datetime, timezone
from typing import Any

from loguru import logger

from echo_agent.agent.tools.base import Tool, ToolExecutionContext, ToolResult, build_idempotency_key


class ToolRegistry:
    """Registry for agent tools with execution, replay guard, and audit logging."""

    def __init__(self):
        self._tools: dict[str, Tool] = {}
        self._replay_cache: dict[str, dict[str, Any]] = {}
        self._execution_log: list[dict[str, Any]] = []

    def register(self, tool: Tool) -> None:
        self._tools[tool.name] = tool

    def unregister(self, name: str) -> None:
        self._tools.pop(name, None)

    def get(self, name: str) -> Tool | None:
        return self._tools.get(name)

    def has(self, name: str) -> bool:
        return name in self._tools

    def get_definitions(self) -> list[dict[str, Any]]:
        return [tool.to_schema() for tool in self._tools.values()]

    @property
    def tool_names(self) -> list[str]:
        return list(self._tools.keys())

    async def execute(
        self,
        name: str,
        params: dict[str, Any],
        ctx: ToolExecutionContext | None = None,
    ) -> ToolResult:
        tool = self._tools.get(name)
        if not tool:
            return ToolResult(success=False, error=f"Tool '{name}' not found. Available: {', '.join(self.tool_names)}")

        errors = tool.validate_params(params)
        if errors:
            return ToolResult(success=False, error=f"Invalid parameters: {'; '.join(errors)}")

        exec_ctx = ctx or ToolExecutionContext(
            execution_id=uuid.uuid4().hex[:12],
            trace_id=uuid.uuid4().hex[:12],
        )

        if tool.execution_mode(params) == "side_effect" and exec_ctx.idempotency_key:
            cached = self._replay_cache.get(exec_ctx.idempotency_key)
            if cached:
                logger.warning("Replay prevented for tool={} key={}", name, exec_ctx.idempotency_key[:16])
                return ToolResult(success=False, error=f"Replay prevented for '{name}'")

        log_entry = {
            "tool": name,
            "params": params,
            "execution_id": exec_ctx.execution_id,
            "trace_id": exec_ctx.trace_id,
            "started_at": datetime.now(timezone.utc).isoformat(),
        }

        attempt = 0
        max_attempts = tool.max_retries + 1
        last_result = ToolResult(success=False, error="no attempt made")

        while attempt < max_attempts:
            try:
                result = await asyncio.wait_for(
                    tool.execute(params, exec_ctx),
                    timeout=tool.timeout_seconds,
                )
                log_entry["completed_at"] = datetime.now(timezone.utc).isoformat()
                log_entry["success"] = result.success
                log_entry["attempt"] = attempt + 1
                self._execution_log.append(log_entry)

                if result.success and tool.execution_mode(params) == "side_effect" and exec_ctx.idempotency_key:
                    self._replay_cache[exec_ctx.idempotency_key] = {
                        "tool": name,
                        "execution_id": exec_ctx.execution_id,
                        "at": datetime.now(timezone.utc).isoformat(),
                    }
                return result
            except asyncio.TimeoutError:
                last_result = ToolResult(success=False, error=f"Tool '{name}' timed out after {tool.timeout_seconds}s")
                logger.warning("Tool {} timed out (attempt {}/{})", name, attempt + 1, max_attempts)
            except Exception as e:
                last_result = ToolResult(success=False, error=f"Tool '{name}' error: {e}")
                logger.error("Tool {} failed (attempt {}/{}): {}", name, attempt + 1, max_attempts, e)
            attempt += 1

        log_entry["completed_at"] = datetime.now(timezone.utc).isoformat()
        log_entry["success"] = False
        log_entry["error"] = last_result.error
        log_entry["attempt"] = attempt
        self._execution_log.append(log_entry)
        return last_result

    def get_execution_log(self, limit: int = 100) -> list[dict[str, Any]]:
        return self._execution_log[-limit:]

    def clear_log(self) -> None:
        self._execution_log.clear()
