"""Delegate tool — orchestrator-worker engine for parallel task delegation.

The main agent (orchestrator) calls this tool when it decides a task benefits from
decomposition or parallel execution. Workers run with tool containment and return
structured results.
"""

from __future__ import annotations

import asyncio
import time
import uuid
from pathlib import Path
from typing import Any

from loguru import logger

from echo_agent.agent.multi_agent.audit import DispatchAuditLog
from echo_agent.agent.multi_agent.models import WorkerProfile, WorkerResult
from echo_agent.agent.multi_agent.registry import WorkerRegistry
from echo_agent.agent.multi_agent.runtime import WorkerExecutor
from echo_agent.agent.tools.base import Tool, ToolExecutionContext, ToolResult
from echo_agent.models.provider import LLMProvider, ToolCallRequest


WORKER_BLOCKED_TOOLS = frozenset({
    "delegate_task",
    "spawn_task",
    "clarify",
    "message",
    "notify",
    "cronjob",
})

_MAX_TOOL_RESULT_CHARS = 16000


class DelegateTool(Tool):
    name = "delegate_task"
    description = (
        "Delegate subtask(s) to worker agent(s) for parallel or isolated execution. "
        "Use when: the task needs parallel research from multiple sources, "
        "the task is complex enough to benefit from decomposition, "
        "or the task requires isolated context to avoid polluting the main conversation. "
        "For simple tasks you can handle directly, do NOT delegate."
    )
    parameters = {
        "type": "object",
        "properties": {
            "tasks": {
                "type": "array",
                "description": "List of subtasks to execute in parallel. Use this for multi-task delegation.",
                "items": {
                    "type": "object",
                    "properties": {
                        "goal": {
                            "type": "string",
                            "description": "Clear task description including expected output format.",
                        },
                        "tools": {
                            "type": "array",
                            "items": {"type": "string"},
                            "description": "Tool names this worker can use (subset of available tools). Omit to use profile defaults or all tools.",
                        },
                        "worker_profile": {
                            "type": "string",
                            "description": "Optional worker template ID (e.g. 'coder', 'researcher', 'operator').",
                        },
                        "max_iterations": {
                            "type": "integer",
                            "description": "Max tool-call iterations for this worker.",
                            "default": 12,
                        },
                    },
                    "required": ["goal"],
                },
            },
            "goal": {
                "type": "string",
                "description": "Single task goal (shorthand for tasks with one item).",
            },
            "tools": {
                "type": "array",
                "items": {"type": "string"},
                "description": "Tools for single-task mode.",
            },
            "worker_profile": {
                "type": "string",
                "description": "Worker template for single-task mode.",
            },
            "max_iterations": {
                "type": "integer",
                "description": "Max iterations for single-task mode.",
                "default": 12,
            },
        },
    }
    risk_level = "exec"
    timeout_seconds = 600

    def __init__(
        self,
        *,
        provider: LLMProvider,
        model_router: Any | None = None,
        tool_registry: Any,
        worker_registry: WorkerRegistry,
        approval_gate: Any,
        credentials: Any,
        audit_path: Path | None = None,
        max_depth: int = 3,
        max_parallel_workers: int = 4,
        max_worker_iterations: int = 12,
        default_model: str = "",
    ):
        self._provider = provider
        self._model_router = model_router
        self._tool_registry = tool_registry
        self._worker_registry = worker_registry
        self._approval_gate = approval_gate
        self._credentials = credentials
        self._audit = DispatchAuditLog(audit_path) if audit_path else None
        self._max_depth = max_depth
        self._max_parallel = max_parallel_workers
        self._max_worker_iterations = max_worker_iterations
        self._default_model = default_model
        self._executor = WorkerExecutor(
            provider=provider,
            model_router=model_router,
            default_model=default_model,
        )

    def execution_mode(self, params: dict[str, Any]) -> str:
        return "side_effect"

    async def execute(self, params: dict[str, Any], ctx: ToolExecutionContext | None = None) -> ToolResult:
        current_depth = self._get_depth(ctx)
        if current_depth >= self._max_depth:
            return ToolResult(
                success=False,
                error=f"Delegation depth limit reached ({self._max_depth}). Handle the task directly.",
            )

        tasks = self._normalize_tasks(params)
        if not tasks:
            return ToolResult(success=False, error="No tasks specified. Provide 'goal' or 'tasks' array.")

        if len(tasks) > self._max_parallel:
            tasks = tasks[:self._max_parallel]
            logger.warning("Truncated delegation to {} parallel workers", self._max_parallel)

        available_tools = set(self._tool_registry.ready_tool_names) - WORKER_BLOCKED_TOOLS
        started = time.monotonic()

        workers = []
        for i, task_spec in enumerate(tasks):
            worker_tools = self._resolve_worker_tools(task_spec, available_tools)
            tool_defs = [
                schema for schema in self._tool_registry.get_ready_definitions()
                if schema.get("function", {}).get("name") in worker_tools
            ]
            profile = self._resolve_profile(task_spec)
            max_iter = min(
                task_spec.get("max_iterations", self._max_worker_iterations),
                self._max_worker_iterations,
            )

            tool_executor = self._make_tool_executor(ctx, worker_tools, current_depth)

            workers.append(self._executor.run(
                task_index=i,
                goal=task_spec["goal"],
                profile=profile,
                tool_defs=tool_defs,
                tool_executor=tool_executor,
                max_iterations=max_iter,
                timeout_seconds=self.timeout_seconds / max(1, len(tasks)),
            ))

        results = await asyncio.gather(*workers, return_exceptions=True)

        worker_results: list[WorkerResult] = []
        for i, r in enumerate(results):
            if isinstance(r, Exception):
                logger.error("Worker {} failed with exception: {}", i, r)
                worker_results.append(WorkerResult(
                    task_index=i,
                    status="failed",
                    error=str(r),
                    duration_seconds=time.monotonic() - started,
                ))
            else:
                worker_results.append(r)

        total_duration = time.monotonic() - started

        if self._audit:
            self._audit.write({
                "type": "delegation",
                "tasks": [t["goal"][:200] for t in tasks],
                "results": [
                    {"index": wr.task_index, "status": wr.status, "iterations": wr.iterations, "tool_calls": wr.tool_calls}
                    for wr in worker_results
                ],
                "total_duration_seconds": round(total_duration, 2),
                "depth": current_depth,
            })

        output = self._format_results(worker_results, total_duration)
        all_success = all(wr.status == "completed" for wr in worker_results)

        return ToolResult(
            success=all_success,
            output=output,
            metadata={
                "worker_count": len(worker_results),
                "total_duration": round(total_duration, 2),
                "depth": current_depth,
            },
        )

    # --- PLACEHOLDER_PRIVATE_METHODS ---

    def _normalize_tasks(self, params: dict[str, Any]) -> list[dict[str, Any]]:
        if "tasks" in params and params["tasks"]:
            return params["tasks"]
        if "goal" in params and params["goal"]:
            return [{
                "goal": params["goal"],
                "tools": params.get("tools"),
                "worker_profile": params.get("worker_profile"),
                "max_iterations": params.get("max_iterations", self._max_worker_iterations),
            }]
        return []

    def _resolve_profile(self, task_spec: dict[str, Any]) -> WorkerProfile | None:
        profile_id = task_spec.get("worker_profile")
        if not profile_id:
            return None
        return self._worker_registry.get(profile_id)

    def _resolve_worker_tools(self, task_spec: dict[str, Any], available: set[str]) -> set[str]:
        requested = task_spec.get("tools")
        if requested:
            return set(requested) & available

        profile = self._resolve_profile(task_spec)
        if profile and profile.default_tools:
            return set(profile.default_tools) & available

        return available

    def _get_depth(self, ctx: ToolExecutionContext | None) -> int:
        if not ctx or not ctx.parent_execution_id:
            return 0
        if ctx.parent_execution_id.startswith("worker:"):
            parts = ctx.parent_execution_id.split(":")
            if len(parts) >= 3:
                try:
                    return int(parts[2]) + 1
                except ValueError:
                    pass
            return 1
        return 0

    def _make_tool_executor(
        self, parent_ctx: ToolExecutionContext | None, allowed_tools: set[str], depth: int
    ) -> Any:
        async def _execute(tool_name: str, tool_call: ToolCallRequest, index: int) -> str:
            if tool_name not in allowed_tools:
                return f"Error: Tool '{tool_name}' is not available for this worker."

            approval_check = await self._approval_gate.check(
                tool_name,
                tool_call.arguments,
                parent_ctx.user_id if parent_ctx else "",
                channel="",
                event=None,
                running=True,
            )
            if approval_check.denial:
                return f"Error: {approval_check.denial.error or approval_check.denial.text}"

            from echo_agent.agent.tools.base import build_idempotency_key
            trace_id = parent_ctx.trace_id if parent_ctx else uuid.uuid4().hex[:12]
            worker_ctx = ToolExecutionContext(
                execution_id=uuid.uuid4().hex[:12],
                trace_id=trace_id,
                session_key=parent_ctx.session_key if parent_ctx else "",
                user_id=parent_ctx.user_id if parent_ctx else "",
                agent_id=f"worker_{depth}",
                attempt_index=0,
                idempotency_key=build_idempotency_key(trace_id, tool_name, index, tool_call.arguments),
                parent_execution_id=f"worker:{tool_name}:{depth}",
                credentials=self._credentials.get_for_tool(tool_name) if self._credentials else {},
                approved_actions=approval_check.approved_actions,
                allowed_tools=frozenset(allowed_tools),
            )
            result = await self._tool_registry.execute(tool_name, tool_call.arguments, worker_ctx)
            text = result.text
            if len(text) > _MAX_TOOL_RESULT_CHARS:
                text = text[:_MAX_TOOL_RESULT_CHARS] + "\n...(truncated)"
            return text

        return _execute

    @staticmethod
    def _format_results(results: list[WorkerResult], total_duration: float) -> str:
        output_parts = []
        for wr in sorted(results, key=lambda r: r.task_index):
            header = f"[Worker {wr.task_index}] status={wr.status}"
            if wr.iterations:
                header += f" iterations={wr.iterations}"
            if wr.tool_calls:
                header += f" tool_calls={wr.tool_calls}"
            header += f" duration={wr.duration_seconds:.1f}s"
            output_parts.append(header)
            if wr.output:
                output_parts.append(wr.output)
            if wr.error:
                output_parts.append(f"Error: {wr.error}")
            output_parts.append("")

        output_parts.append(f"Total duration: {total_duration:.1f}s")
        return "\n".join(output_parts)


class SpawnTool(Tool):
    """Spawn a background task that runs asynchronously."""

    name = "spawn_task"
    description = (
        "Spawn a background task that runs asynchronously. "
        "Returns immediately with a task ID. The result will be announced when complete."
    )
    parameters = {
        "type": "object",
        "properties": {
            "task": {"type": "string", "description": "Description of the task to run in background."},
            "context": {"type": "string", "description": "Additional context for the background agent."},
        },
        "required": ["task"],
    }

    def __init__(self, provider: LLMProvider, bus: Any = None):
        self._provider = provider
        self._bus = bus
        self._tasks: dict[str, asyncio.Task] = {}

    def execution_mode(self, params: dict[str, Any]) -> str:
        return "side_effect"

    async def execute(self, params: dict[str, Any], ctx: ToolExecutionContext | None = None) -> ToolResult:
        task_desc = params["task"]
        extra = params.get("context", "")
        task_id = f"bg_{uuid.uuid4().hex[:8]}"

        async_task = asyncio.create_task(self._run_background(task_id, task_desc, extra, ctx))
        self._tasks[task_id] = async_task
        return ToolResult(
            output=f"Background task '{task_id}' started. Result will be announced when complete.",
            metadata={"task_id": task_id},
        )

    async def _run_background(self, task_id: str, task: str, context: str, ctx: ToolExecutionContext | None) -> None:
        system = "You are a background agent. Complete the task and provide a concise result."
        if context:
            system += f"\n\nContext:\n{context}"

        messages: list[dict[str, Any]] = [
            {"role": "system", "content": system},
            {"role": "user", "content": task},
        ]

        try:
            resp = await self._provider.chat_with_retry(messages=messages)
            result = resp.content or "(no response)"
            logger.info("Background task {} completed: {}", task_id, result[:100])

            if self._bus:
                from echo_agent.bus.events import InboundEvent
                announce = InboundEvent.text_message(
                    channel="system", sender_id="system",
                    chat_id=ctx.session_key.split(":")[1] if ctx else "system",
                    text=f"[Background task {task_id} completed]\n\n{result}",
                    session_key_override=ctx.session_key if ctx else "system:system",
                )
                await self._bus.publish_inbound(announce)
        except Exception as e:
            logger.error("Background task {} failed: {}", task_id, e)
        finally:
            self._tasks.pop(task_id, None)
