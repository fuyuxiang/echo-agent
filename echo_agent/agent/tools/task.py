"""Agent-facing task tool — DB-backed task management with state machine."""

from __future__ import annotations

import json
from typing import Any

from loguru import logger

from echo_agent.agent.tools.base import Tool, ToolExecutionContext, ToolResult
from echo_agent.tasks.manager import TaskManager
from echo_agent.tasks.models import TaskStatus


class TaskTool(Tool):
    name = "task"
    description = (
        "Manage tasks with full lifecycle tracking. Actions: "
        "create, list, get, start, complete, fail, cancel, retry, update. "
        "Workflow steps are queued as tasks carrying tool_name/tool_params in "
        "metadata: YOU are the executor — 'start' the task, run the tool "
        "yourself, then 'complete' (or 'fail') it; completing a workflow step "
        "automatically advances its workflow to queue the next steps."
    )
    parameters = {
        "type": "object",
        "properties": {
            "action": {
                "type": "string",
                "enum": ["create", "list", "get", "start", "complete", "fail", "cancel", "retry", "update"],
                "description": "Action to perform",
            },
            "task_id": {"type": "string", "description": "Task ID (for get/start/complete/fail/cancel/retry/update)"},
            "title": {"type": "string", "description": "Task title (for create/update)"},
            "description": {"type": "string", "description": "Task description (for create/update)"},
            "result": {"type": "string", "description": "Result summary (for complete)"},
            "error": {"type": "string", "description": "Error message (for fail)"},
            "status_filter": {"type": "string", "description": "Filter by status (for list)"},
            "workflow_id": {"type": "string", "description": "Filter by workflow (for list) or assign (for create)"},
            "priority": {"type": "integer", "description": "Priority 0-9 (for create/update)"},
        },
        "required": ["action"],
    }

    def __init__(self, manager: TaskManager, workflow_engine: Any = None):
        self._mgr = manager
        # 步骤任务完成/失败后自动推进所属工作流(on_task_complete→advance)。
        # 不注入时保持旧行为:完成任务不触碰工作流状态。
        self._workflow_engine = workflow_engine

    async def _advance_workflow(self, task: Any) -> None:
        """Best-effort workflow advance after a terminal task transition. The
        task state change has already been persisted — a failed advance must
        not fail the tool call, it only delays DAG progress until the next
        explicit 'workflow advance'."""
        if self._workflow_engine is None or not getattr(task, "workflow_id", ""):
            return
        try:
            await self._workflow_engine.on_task_complete(task.id)
        except Exception as e:
            logger.warning(
                "Workflow advance after task {} failed: {}", task.id, e
            )

    async def execute(self, params: dict[str, Any], ctx: ToolExecutionContext | None = None) -> ToolResult:
        action = params["action"]
        try:
            if action == "create":
                task = await self._mgr.create(
                    title=params.get("title", "Untitled"),
                    description=params.get("description", ""),
                    workflow_id=params.get("workflow_id", ""),
                    priority=params.get("priority", 5),
                )
                return ToolResult(output=f"Task created: {task.id} '{task.title}'")

            if action == "list":
                sf = params.get("status_filter", "")
                wf = params.get("workflow_id", "")
                if wf:
                    tasks = await self._mgr.list_by_workflow(wf)
                elif sf:
                    tasks = await self._mgr.list_by_status(TaskStatus(sf))
                else:
                    tasks = await self._mgr.list_by_status()
                if not tasks:
                    return ToolResult(output="No tasks found.")
                lines = [f"[{t.status.value}] {t.id}: {t.title}" for t in tasks[:50]]
                return ToolResult(output="\n".join(lines))

            if action == "get":
                task = await self._mgr.get(params.get("task_id", ""))
                if not task:
                    return ToolResult(success=False, error="Task not found")
                return ToolResult(output=json.dumps(task.to_dict(), ensure_ascii=False, indent=2))

            if action == "start":
                task = await self._mgr.transition(params["task_id"], TaskStatus.RUNNING)
                return ToolResult(output=f"Task {task.id} started")

            if action == "complete":
                # 状态机不允许 RUNNING→SUCCESS 直达(须经 REVIEW)。agent 视角的
                # "complete"就是"做完了",自动补上 REVIEW 过渡——否则 complete
                # 对 RUNNING 任务必然抛 Invalid transition,驱动闭环卡死。
                task = await self._mgr.get(params["task_id"])
                if task is not None and task.status == TaskStatus.RUNNING:
                    await self._mgr.transition(params["task_id"], TaskStatus.REVIEW)
                task = await self._mgr.transition(params["task_id"], TaskStatus.SUCCESS, result=params.get("result", ""))
                await self._advance_workflow(task)
                return ToolResult(output=f"Task {task.id} completed")

            if action == "fail":
                task = await self._mgr.transition(params["task_id"], TaskStatus.FAILED, error=params.get("error", ""))
                await self._advance_workflow(task)
                return ToolResult(output=f"Task {task.id} failed")

            if action == "cancel":
                task = await self._mgr.cancel(params["task_id"])
                return ToolResult(output=f"Task {task.id} cancelled")

            if action == "retry":
                task = await self._mgr.retry(params["task_id"])
                return ToolResult(output=f"Task {task.id} retrying (attempt {task.retry_count})")

            if action == "update":
                fields = {}
                for k in ("title", "description", "priority"):
                    if k in params:
                        fields[k] = params[k]
                task = await self._mgr.update(params["task_id"], **fields)
                return ToolResult(output=f"Task {task.id} updated")

            return ToolResult(success=False, error=f"Unknown action: {action}")
        except ValueError as e:
            return ToolResult(success=False, error=str(e))
