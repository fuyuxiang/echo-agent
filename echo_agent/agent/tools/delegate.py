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
from echo_agent.agent.multi_agent.models import WorkerProfile, WorkerResult, WorkerToolOutcome
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

# spawn_task runs a self-contained background agent whose whole purpose is to
# "go land one thing" on the user's behalf (write a file, register a cron job,
# run a command). Unlike delegate workers — which report findings back to the
# orchestrator — a spawned worker IS the actor, so it is allowed to schedule
# cron jobs. spawn/delegate/clarify are still blocked: they are the recursion
# vectors (a spawned worker must not spawn again — that was the message storm
# c2afd52 tried to kill). message/notify stay blocked so the worker cannot talk
# to the user directly; its result is delivered once, by SpawnTool itself.
SPAWN_BLOCKED_TOOLS = frozenset({
    "delegate_task",
    "spawn_task",
    "clarify",
    "message",
    "notify",
})

_MAX_TOOL_RESULT_CHARS = 16000


def build_worker_tool_executor(
    *,
    tool_registry: Any,
    approval_gate: Any,
    credentials: Any,
    parent_ctx: ToolExecutionContext | None,
    allowed_tools: set[str],
    depth: int,
) -> Any:
    """Build a tool executor that runs worker tool calls through the real
    ToolRegistry, gated by the approval flow and contained to ``allowed_tools``.

    Shared by DelegateTool (orchestrator workers) and SpawnTool (background
    workers) so both honour the exact same approval/containment contract.
    """

    async def _execute(tool_name: str, tool_call: ToolCallRequest, index: int) -> WorkerToolOutcome:
        if tool_name not in allowed_tools:
            return WorkerToolOutcome(
                text=f"Error: Tool '{tool_name}' is not available for this worker.",
                success=False,
            )

        # Gate the worker's call on the SAME facts as the turn that dispatched it.
        # This used to pass channel="" and event=None, which the gate read as a
        # local human-at-the-keyboard session (""  is in _INTERACTIVE_CHANNELS):
        # a worker's exec auto-approved on cli_auto_approve even when the parent
        # call arrived from telegram and would itself have needed consent, and a
        # scheduled job's worker looked interactive rather than unattended. The
        # dispatch is gated too (delegate/spawn are EXEC-tier), but a worker must
        # not be able to reach further than whoever dispatched it.
        #
        # nested=True tells the gate no keyboard is behind this call, so the
        # interactive shortcuts do not apply regardless of the channel string.
        approval_check = await approval_gate.check(
            tool_name,
            tool_call.arguments,
            parent_ctx.user_id if parent_ctx else "",
            channel=parent_ctx.channel if parent_ctx else "",
            event=None,
            running=True,
            unattended=bool(parent_ctx.unattended) if parent_ctx else False,
            cron_authorized=bool(parent_ctx.cron_authorized) if parent_ctx else False,
            nested=True,
        )
        if approval_check.denial:
            return WorkerToolOutcome(
                text=f"Error: {approval_check.denial.error or approval_check.denial.text}",
                success=False,
            )

        from echo_agent.agent.tools.base import build_idempotency_key
        trace_id = parent_ctx.trace_id if parent_ctx else uuid.uuid4().hex[:12]
        worker_ctx = ToolExecutionContext(
            execution_id=uuid.uuid4().hex[:12],
            trace_id=trace_id,
            session_key=parent_ctx.session_key if parent_ctx else "",
            memory_scope=parent_ctx.memory_scope if parent_ctx else "",
            user_id=parent_ctx.user_id if parent_ctx else "",
            agent_id=f"worker_{depth}",
            attempt_index=0,
            idempotency_key=build_idempotency_key(trace_id, tool_name, index, tool_call.arguments),
            parent_execution_id=f"worker:{tool_name}:{depth}",
            credentials=credentials.get_for_tool(tool_name) if credentials else {},
            approved_actions=approval_check.approved_actions,
            approval_source=approval_check.approval_source,
            allowed_tools=frozenset(allowed_tools),
            # Carry the origin and trust facts down, so a tool that itself
            # consults them (or a deeper nesting level) sees the dispatching
            # turn's context rather than a blank one.
            channel=parent_ctx.channel if parent_ctx else "",
            chat_id=parent_ctx.chat_id if parent_ctx else "",
            unattended=bool(parent_ctx.unattended) if parent_ctx else False,
            cron_authorized=bool(parent_ctx.cron_authorized) if parent_ctx else False,
        )
        result = await tool_registry.execute(tool_name, tool_call.arguments, worker_ctx)
        text = result.text
        if len(text) > _MAX_TOOL_RESULT_CHARS:
            text = text[:_MAX_TOOL_RESULT_CHARS] + "\n...(truncated)"
        # Carry success through instead of collapsing to text: the worker loop
        # uses it to stop burning iterations on a tool that keeps failing.
        return WorkerToolOutcome(text=text, success=bool(getattr(result, "success", True)))

    return _execute


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
        return build_worker_tool_executor(
            tool_registry=self._tool_registry,
            approval_gate=self._approval_gate,
            credentials=self._credentials,
            parent_ctx=parent_ctx,
            allowed_tools=allowed_tools,
            depth=depth,
        )

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
    """Spawn a background worker agent that runs asynchronously WITH tools.

    Unlike a bare chat completion, the spawned worker executes real tool calls
    (exec, write_file, cronjob, ...) through the shared ToolRegistry, gated by
    the same approval flow delegate workers use. It is the actor for the task,
    not a planner: its final result reflects side effects actually performed,
    so a spawned "set a daily reminder" task really writes scheduler.json.
    """

    name = "spawn_task"
    description = (
        "Spawn a background worker that runs asynchronously and CAN use tools "
        "(run commands, write files, schedule cron jobs). Returns immediately "
        "with a task ID; the real result is delivered when the worker finishes. "
        "Use for fire-and-forget work that must actually take effect in the "
        "background. The worker cannot spawn or delegate further."
    )
    parameters = {
        "type": "object",
        "properties": {
            "task": {"type": "string", "description": "Description of the task to run in background. Include the expected concrete outcome (e.g. 'schedule a 6:30 daily job')."},
            "context": {"type": "string", "description": "Additional context for the background worker."},
        },
        "required": ["task"],
    }
    risk_level = "exec"

    def __init__(
        self,
        provider: LLMProvider,
        bus: Any = None,
        *,
        tool_registry: Any = None,
        approval_gate: Any = None,
        credentials: Any = None,
        model_router: Any | None = None,
        default_model: str = "",
        max_iterations: int = 12,
        timeout_seconds: float = 300.0,
    ):
        self._provider = provider
        self._bus = bus
        self._tool_registry = tool_registry
        self._approval_gate = approval_gate
        self._credentials = credentials
        self._default_model = default_model
        self._max_iterations = max_iterations
        self._timeout_seconds = timeout_seconds
        self._executor = WorkerExecutor(
            provider=provider,
            model_router=model_router,
            default_model=default_model,
        )
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
            output=f"Background task '{task_id}' started. The result will be delivered when the worker finishes.",
            metadata={"task_id": task_id},
        )

    def _worker_tools(self) -> tuple[set[str], list[dict[str, Any]]]:
        """Resolve the tool set and schemas the background worker may use."""
        if not self._tool_registry:
            return set(), []
        allowed = set(self._tool_registry.ready_tool_names) - SPAWN_BLOCKED_TOOLS
        tool_defs = [
            schema for schema in self._tool_registry.get_ready_definitions()
            if schema.get("function", {}).get("name") in allowed
        ]
        return allowed, tool_defs

    async def _run_background(self, task_id: str, task: str, context: str, ctx: ToolExecutionContext | None) -> None:
        try:
            result = await self._execute_worker(task_id, task, context, ctx)
            # Collapse newlines: a multi-line result (a failed worker appends its
            # error on its own line) otherwise spills past the timestamped first
            # line and reads like stray un-logged stderr output.
            logger.info(
                "Background task {} completed: {}",
                task_id, " ".join(result.split())[:300],
            )
            await self._announce(task_id, result, ctx)
        except Exception as e:
            logger.error("Background task {} failed: {}", task_id, e)
            await self._announce(task_id, f"任务执行失败：{e}", ctx)
        finally:
            self._tasks.pop(task_id, None)

    async def _execute_worker(self, task_id: str, task: str, context: str, ctx: ToolExecutionContext | None) -> str:
        # No tool registry wired (e.g. multi-agent disabled path): fall back to a
        # plain completion so the tool still answers, but say so honestly.
        if not self._tool_registry or not self._approval_gate:
            resp = await self._provider.chat_with_retry(messages=[
                {"role": "system", "content": "You are a background agent. Complete the task and provide a concise result."},
                {"role": "user", "content": f"{task}\n\n{context}".strip()},
            ])
            return resp.content or "(no response)"

        allowed_tools, tool_defs = self._worker_tools()
        tool_executor = build_worker_tool_executor(
            tool_registry=self._tool_registry,
            approval_gate=self._approval_gate,
            credentials=self._credentials,
            parent_ctx=ctx,
            allowed_tools=allowed_tools,
            depth=1,
        )
        instructions = (
            "You are a background worker executing a task on the user's behalf. "
            "You MUST actually perform the work using the available tools — do not "
            "merely describe a plan. After acting, verify the effect took place "
            "(e.g. read back the file/row you wrote) and report the concrete result. "
            "If a required tool is unavailable or denied, say so plainly; never "
            "claim success you did not achieve."
        )
        if context:
            instructions += f"\n\nContext:\n{context}"

        worker_result = await self._executor.run(
            task_index=0,
            goal=task,
            system_prompt=instructions,
            tool_defs=tool_defs,
            tool_executor=tool_executor,
            max_iterations=self._max_iterations,
            timeout_seconds=self._timeout_seconds,
        )
        output = worker_result.output or "(no result)"
        if worker_result.status != "completed":
            output = f"[{worker_result.status}] {output}"
            if worker_result.error:
                output += f"\n{worker_result.error}"
        return output

    async def _announce(self, task_id: str, result: str, ctx: ToolExecutionContext | None) -> None:
        if not self._bus:
            return
        from echo_agent.bus.events import OutboundEvent
        from echo_agent.scheduler.delivery import target_from_session_key

        if ctx and ctx.session_key:
            channel, chat_id = target_from_session_key(ctx.session_key)
        else:
            channel, chat_id = "", ""
        channel = channel or "system"
        chat_id = chat_id or "system"

        announce = OutboundEvent.from_text_with_media(
            channel=channel,
            chat_id=chat_id,
            text=f"[Background task {task_id} completed]\n\n{result}",
        )
        announce.metadata["_background_result"] = True
        await self._bus.publish_outbound(announce)
