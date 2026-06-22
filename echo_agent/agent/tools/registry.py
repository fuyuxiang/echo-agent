"""Tool registry — dynamic registration, permission checks, execution with retry/timeout/logging."""

from __future__ import annotations

import asyncio
import collections
import json
import uuid
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from loguru import logger

from echo_agent.agent.tools.base import Tool, ToolExecutionContext, ToolResult

_MAX_REPLAY_CACHE = 500
_MAX_EXECUTION_LOG = 1000
_MAX_AUDIT_FILE_BYTES = 5_000_000

_SENSITIVE_KEYS = frozenset({"key", "token", "secret", "password", "api_key", "credential", "auth"})


def _mask_sensitive(params: dict[str, Any]) -> dict[str, Any]:
    masked: dict[str, Any] = {}
    for k, v in params.items():
        if any(s in k.lower() for s in _SENSITIVE_KEYS):
            masked[k] = "***"
        elif isinstance(v, dict):
            masked[k] = _mask_sensitive(v)
        else:
            masked[k] = v
    return masked


class ToolRegistry:
    """Registry for agent tools with execution, replay guard, and audit logging."""

    _ALIASES: dict[str, str] = {
        "bash": "exec",
        "shell": "exec",
        "run_code": "execute_code",
        "code": "execute_code",
    }

    def __init__(self, audit_log_path: Path | None = None, config: Any = None):
        self._tools: dict[str, Tool] = {}
        self._replay_cache: collections.OrderedDict[str, dict[str, Any]] = collections.OrderedDict()
        self._execution_log: collections.deque[dict[str, Any]] = collections.deque(maxlen=_MAX_EXECUTION_LOG)
        self._lock = asyncio.Lock()
        # Durable JSONL audit trail — the in-memory deque alone evaporates on
        # restart, which defeats the point of an audit log.
        self._audit_log_path = audit_log_path
        # Defense-in-depth: registration-time filtering (filter_tools_by_policy)
        # is the primary gate, but if the security profile is tightened *after*
        # registration the registry would still hold high-risk tools. Re-checking
        # the policy at execute time closes that window. When config is set, every
        # tool that passed registration already satisfies this check under the
        # same config, so this is a no-op unless the profile changed underneath.
        self._config = config

    def set_audit_log_path(self, path: Path) -> None:
        self._audit_log_path = path

    def _append_audit(self, entry: dict[str, Any]) -> None:
        if self._audit_log_path is None:
            return
        try:
            self._audit_log_path.parent.mkdir(parents=True, exist_ok=True)
            if (
                self._audit_log_path.exists()
                and self._audit_log_path.stat().st_size > _MAX_AUDIT_FILE_BYTES
            ):
                rotated = self._audit_log_path.with_name(
                    f"{self._audit_log_path.stem}-{datetime.now(timezone.utc).strftime('%Y%m%d%H%M%S')}.jsonl"
                )
                self._audit_log_path.replace(rotated)
            with self._audit_log_path.open("a", encoding="utf-8") as f:
                f.write(json.dumps(entry, ensure_ascii=False, default=str) + "\n")
        except Exception as e:
            logger.debug("Failed to append tool audit entry: {}", e)

    def _resolve(self, name: str) -> str:
        return self._ALIASES.get(name, name)

    def register(self, tool: Tool) -> None:
        self._tools[tool.name] = tool

    def unregister(self, name: str) -> None:
        self._tools.pop(name, None)

    def get(self, name: str) -> Tool | None:
        return self._tools.get(self._resolve(name))

    def has(self, name: str) -> bool:
        return self._resolve(name) in self._tools

    def get_definitions(self) -> list[dict[str, Any]]:
        definitions: list[dict[str, Any]] = []
        for tool in self._tools.values():
            try:
                definitions.append(tool.to_schema())
            except ValueError as e:
                logger.error("Skipping tool '{}' due to invalid schema: {}", tool.name, e)
        return definitions

    def get_ready_definitions(self) -> list[dict[str, Any]]:
        """Like get_definitions() but only includes tools where is_ready() is True."""
        definitions: list[dict[str, Any]] = []
        for tool in self._tools.values():
            if not tool.is_ready():
                continue
            try:
                definitions.append(tool.to_schema())
            except ValueError as e:
                logger.error("Skipping tool '{}' due to invalid schema: {}", tool.name, e)
        return definitions

    @property
    def ready_tool_names(self) -> list[str]:
        return [name for name, tool in self._tools.items() if tool.is_ready()]

    def get_readiness_report(self) -> list[tuple[str, bool, str]]:
        """Returns [(tool_name, ready, reason), ...] for all registered tools."""
        return [(name, *tool.readiness_detail()) for name, tool in self._tools.items()]

    @property
    def tool_names(self) -> list[str]:
        return list(self._tools.keys())

    async def execute(
        self,
        name: str,
        params: dict[str, Any],
        ctx: ToolExecutionContext | None = None,
        *,
        replay_scope: str = "",
    ) -> ToolResult:
        resolved_name = self._resolve(name)
        if ctx and ctx.allowed_tools and resolved_name not in ctx.allowed_tools:
            return ToolResult(success=False, error=f"Tool '{name}' is outside the scoped tool allowlist")

        tool = self._tools.get(resolved_name)
        if not tool:
            return ToolResult(success=False, error=f"Tool '{name}' not found. Available: {', '.join(self.tool_names)}")

        # Defense-in-depth re-check: native tools have no other runtime policy
        # gate (only mcp_* tools are re-checked in the loop). If the security
        # profile was tightened after registration, refuse the call here even
        # though the tool is still in the registry.
        if self._config is not None:
            from echo_agent.security.tool_policy import is_tool_allowed
            if not is_tool_allowed(self._config, tool):
                logger.warning("Tool '{}' blocked at execute time by security policy", resolved_name)
                return ToolResult(success=False, error=f"Tool '{name}' is not allowed under the current security profile")

        errors = tool.validate_params(params)
        if errors:
            return ToolResult(success=False, error=f"Invalid parameters: {'; '.join(errors)}")

        exec_ctx = ctx or ToolExecutionContext(
            execution_id=uuid.uuid4().hex[:12],
            trace_id=uuid.uuid4().hex[:12],
        )

        if tool.execution_mode(params) == "side_effect" and exec_ctx.idempotency_key:
            effective_key = f"{replay_scope}:{exec_ctx.idempotency_key}" if replay_scope else exec_ctx.idempotency_key
            async with self._lock:
                cached = self._replay_cache.get(effective_key)
            if cached and not exec_ctx.is_replay:
                # Honour explicit replay requests; otherwise refuse to repeat
                # a side-effecting call with the same idempotency key.
                logger.warning("Replay prevented for tool={} key={}", name, effective_key[:16])
                return ToolResult(success=False, error=f"Replay prevented for '{name}'")

        log_entry = {
            "tool": name,
            "params": _mask_sensitive(params),
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
                self._append_audit(log_entry)

                if result.success and tool.execution_mode(params) == "side_effect" and exec_ctx.idempotency_key:
                    effective_key = f"{replay_scope}:{exec_ctx.idempotency_key}" if replay_scope else exec_ctx.idempotency_key
                    async with self._lock:
                        self._replay_cache[effective_key] = {
                            "tool": name,
                            "execution_id": exec_ctx.execution_id,
                            "at": datetime.now(timezone.utc).isoformat(),
                        }
                        while len(self._replay_cache) > _MAX_REPLAY_CACHE:
                            self._replay_cache.popitem(last=False)
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
        self._append_audit(log_entry)
        return last_result

    def get_execution_log(self, limit: int = 100) -> list[dict[str, Any]]:
        return list(self._execution_log)[-limit:]

    def clear_log(self) -> None:
        self._execution_log.clear()
