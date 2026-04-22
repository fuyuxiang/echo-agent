"""Workflow engine — DAG-based multi-step orchestration on top of TaskManager."""

from __future__ import annotations

from typing import Any

from loguru import logger

from echo_agent.tasks.models import (
    WorkflowRecord,
    WorkflowStatus,
    StepDefinition,
    TaskStatus,
    VALID_WORKFLOW_TRANSITIONS,
    TERMINAL_TASK_STATUSES,
    TERMINAL_WORKFLOW_STATUSES,
    _now,
)
from echo_agent.tasks.manager import TaskManager


class WorkflowEngine:
    """Manages workflow lifecycle and DAG step resolution."""

    def __init__(self, storage: Any, task_manager: TaskManager):
        self._storage = storage
        self._tasks = task_manager

    async def create(
        self,
        name: str,
        steps: list[dict[str, Any]],
        description: str = "",
    ) -> WorkflowRecord:
        step_defs = []
        for i, s in enumerate(steps):
            sd = StepDefinition.from_dict(s)
            if not sd.id:
                sd.id = f"step_{i}"
            if not sd.name:
                sd.name = sd.tool_name or sd.id
            step_defs.append(sd)
        wf = WorkflowRecord(name=name, description=description, steps=step_defs)
        await self._storage.store_workflow(wf.id, wf.to_dict())
        logger.info("Workflow created: {} '{}'", wf.id, name)
        return wf

    async def get(self, workflow_id: str) -> WorkflowRecord | None:
        data = await self._storage.load_workflow(workflow_id)
        if not data:
            return None
        return WorkflowRecord.from_dict(data)

    async def _save(self, wf: WorkflowRecord) -> None:
        wf.updated_at = _now()
        await self._storage.store_workflow(wf.id, wf.to_dict())

    def _transition(self, wf: WorkflowRecord, new_status: WorkflowStatus) -> None:
        allowed = VALID_WORKFLOW_TRANSITIONS.get(wf.status, set())
        if new_status not in allowed:
            raise ValueError(f"Invalid workflow transition: {wf.status.value} → {new_status.value}")
        wf.status = new_status

    async def start(self, workflow_id: str) -> WorkflowRecord:
        wf = await self.get(workflow_id)
        if not wf:
            raise ValueError(f"Workflow '{workflow_id}' not found")
        self._transition(wf, WorkflowStatus.RUNNING)
        await self._queue_eligible_steps(wf)
        await self._save(wf)
        logger.info("Workflow {} started", workflow_id)
        return wf

    async def advance(self, workflow_id: str) -> WorkflowRecord:
        wf = await self.get(workflow_id)
        if not wf:
            raise ValueError(f"Workflow '{workflow_id}' not found")
        if wf.status in TERMINAL_WORKFLOW_STATUSES:
            return wf

        tasks = await self._tasks.list_by_workflow(workflow_id)
        task_map = {t.id: t for t in tasks}
        step_status: dict[str, TaskStatus | None] = {}
        for step in wf.steps:
            tid = wf.step_tasks.get(step.id)
            step_status[step.id] = task_map[tid].status if tid and tid in task_map else None

        any_failed = any(s == TaskStatus.FAILED for s in step_status.values())
        all_done = all(
            s in TERMINAL_TASK_STATUSES
            for s in step_status.values()
            if s is not None
        ) and len(step_status) > 0 and all(s is not None for s in step_status.values())

        if any_failed:
            self._transition(wf, WorkflowStatus.FAILED)
            await self._save(wf)
            return wf

        if all_done:
            self._transition(wf, WorkflowStatus.SUCCESS)
            await self._save(wf)
            logger.info("Workflow {} completed successfully", workflow_id)
            return wf

        await self._queue_eligible_steps(wf)
        await self._save(wf)
        return wf

    async def on_task_complete(self, task_id: str) -> None:
        task = await self._tasks.get(task_id)
        if not task or not task.workflow_id:
            return
        await self.advance(task.workflow_id)

    async def pause(self, workflow_id: str) -> WorkflowRecord:
        wf = await self.get(workflow_id)
        if not wf:
            raise ValueError(f"Workflow '{workflow_id}' not found")
        self._transition(wf, WorkflowStatus.WAITING)
        await self._save(wf)
        return wf

    async def resume(self, workflow_id: str) -> WorkflowRecord:
        wf = await self.get(workflow_id)
        if not wf:
            raise ValueError(f"Workflow '{workflow_id}' not found")
        self._transition(wf, WorkflowStatus.RUNNING)
        await self._queue_eligible_steps(wf)
        await self._save(wf)
        return wf

    async def cancel(self, workflow_id: str) -> WorkflowRecord:
        wf = await self.get(workflow_id)
        if not wf:
            raise ValueError(f"Workflow '{workflow_id}' not found")
        self._transition(wf, WorkflowStatus.CANCELLED)
        for tid in wf.step_tasks.values():
            t = await self._tasks.get(tid)
            if t and t.status not in TERMINAL_TASK_STATUSES:
                try:
                    await self._tasks.cancel(tid)
                except ValueError:
                    pass
        await self._save(wf)
        return wf

    async def list_all(self, status: str | None = None) -> list[WorkflowRecord]:
        rows = await self._storage.list_workflows(status=status)
        return [WorkflowRecord.from_dict(r) for r in rows]

    async def _queue_eligible_steps(self, wf: WorkflowRecord) -> None:
        tasks = await self._tasks.list_by_workflow(wf.id)
        task_map = {t.id: t for t in tasks}
        completed_steps: set[str] = set()
        active_steps: set[str] = set()
        for step in wf.steps:
            tid = wf.step_tasks.get(step.id)
            if tid and tid in task_map:
                t = task_map[tid]
                if t.status == TaskStatus.SUCCESS:
                    completed_steps.add(step.id)
                elif t.status not in TERMINAL_TASK_STATUSES:
                    active_steps.add(step.id)

        for step in wf.steps:
            if step.id in completed_steps or step.id in active_steps:
                continue
            if wf.step_tasks.get(step.id):
                continue
            deps_met = all(d in completed_steps for d in step.depends_on)
            if not deps_met:
                continue
            task = await self._tasks.create(
                title=step.name,
                description=f"Workflow step: {step.tool_name}({step.tool_params})",
                workflow_id=wf.id,
                max_retries=step.retry_max,
                metadata={"step_id": step.id, "tool_name": step.tool_name, "tool_params": step.tool_params},
            )
            wf.step_tasks[step.id] = task.id
            wf.current_step = step.id
