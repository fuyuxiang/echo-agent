"""Workflow engine — DAG-based multi-step orchestration on top of TaskManager."""

from __future__ import annotations

import asyncio
from functools import wraps
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


def _serialized(method):
    """One Runtime owns this engine; serialize its read/create/save sequences."""
    @wraps(method)
    async def locked(self, *args, **kwargs):
        async with self._mutation_lock:
            return await method(self, *args, **kwargs)
    return locked


class WorkflowEngine:
    """Manages workflow lifecycle and DAG step resolution.

    Orchestration only: this engine creates and tracks step tasks and resolves
    DAG dependencies, but does NOT execute the steps' tools. TaskManager has no
    executor — queued step tasks carry tool_name/tool_params in metadata for an
    external driver to run, then call advance() to progress the workflow.
    """

    def __init__(self, storage: Any, task_manager: TaskManager):
        self._storage = storage
        self._tasks = task_manager
        self._mutation_lock = asyncio.Lock()

    @staticmethod
    def _validate_steps(steps: list[StepDefinition]) -> None:
        if not steps:
            raise ValueError("Workflow requires at least one step")
        ids = [step.id for step in steps]
        if any(not isinstance(sid, str) or not sid.strip() for sid in ids):
            raise ValueError("Workflow step IDs must be nonempty strings")
        if len(set(ids)) != len(ids):
            raise ValueError("Workflow step IDs must be unique")
        known = set(ids)
        for step in steps:
            if (not isinstance(step.depends_on, list)
                    or any(not isinstance(dep, str) or dep not in known for dep in step.depends_on)):
                raise ValueError(f"Step '{step.id}' has an unknown or invalid dependency")
            if step.condition:
                raise ValueError("Workflow conditions are not supported; use explicit Agent task dependencies")
            if not isinstance(step.tool_params, dict):
                raise ValueError(f"Step '{step.id}' tool_params must be an object")
            if type(step.retry_max) is not int or step.retry_max < 0:
                raise ValueError(f"Step '{step.id}' retry_max must be a nonnegative integer")
            if type(step.timeout_seconds) is not int or step.timeout_seconds <= 0:
                raise ValueError(f"Step '{step.id}' timeout_seconds must be a positive integer")
        # Kahn's algorithm also rejects self-dependencies, without recursive
        # traversal of user-supplied graphs.
        remaining = {step.id: set(step.depends_on) for step in steps}
        while remaining:
            ready = {sid for sid, deps in remaining.items() if not deps}
            if not ready:
                raise ValueError("Workflow dependencies contain a cycle")
            remaining = {sid: deps - ready for sid, deps in remaining.items() if sid not in ready}

    async def create(
        self,
        name: str,
        steps: list[dict[str, Any]],
        description: str = "",
    ) -> WorkflowRecord:
        if not isinstance(steps, list) or any(not isinstance(step, dict) for step in steps):
            raise ValueError("Workflow steps must be a list of objects")
        step_defs = []
        for i, s in enumerate(steps):
            sd = StepDefinition.from_dict(s)
            if not sd.id:
                sd.id = f"step_{i}"
            if not sd.name:
                sd.name = sd.tool_name or sd.id
            step_defs.append(sd)
        self._validate_steps(step_defs)
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

    @_serialized
    async def start(self, workflow_id: str) -> WorkflowRecord:
        wf = await self.get(workflow_id)
        if not wf:
            raise ValueError(f"Workflow '{workflow_id}' not found")
        self._validate_steps(wf.steps)
        self._transition(wf, WorkflowStatus.RUNNING)
        await self._queue_eligible_steps(wf)
        await self._save(wf)
        logger.info("Workflow {} started", workflow_id)
        return wf

    @_serialized
    async def advance(self, workflow_id: str) -> WorkflowRecord:
        return await self._advance(workflow_id)

    async def _advance(self, workflow_id: str) -> WorkflowRecord:
        wf = await self.get(workflow_id)
        if not wf:
            raise ValueError(f"Workflow '{workflow_id}' not found")
        if wf.status in TERMINAL_WORKFLOW_STATUSES:
            return wf
        # Pausing prevents further dispatch; already executing steps may finish.
        # Their outcomes are reconciled on resume, never by queuing while paused.
        if wf.status != WorkflowStatus.RUNNING:
            return wf
        self._validate_steps(wf.steps)

        tasks = await self._tasks.list_by_workflow(workflow_id)
        task_map = {t.id: t for t in tasks}
        step_status: dict[str, TaskStatus | None] = {}
        for step in wf.steps:
            tid = wf.step_tasks.get(step.id)
            step_status[step.id] = task_map[tid].status if tid and tid in task_map else None

        for step in wf.steps:
            task = task_map.get(wf.step_tasks.get(step.id, ""))
            if task and task.status == TaskStatus.FAILED and task.retry_count < step.retry_max:
                retried = await self._tasks.retry(task.id)
                step_status[step.id] = retried.status
        any_failed = any(s == TaskStatus.FAILED for s in step_status.values())
        any_cancelled = any(s == TaskStatus.CANCELLED for s in step_status.values())
        all_done = bool(step_status) and all(s == TaskStatus.SUCCESS for s in step_status.values())

        if any_failed:
            self._transition(wf, WorkflowStatus.FAILED)
            await self._cancel_steps(wf)
            await self._save(wf)
            return wf

        if any_cancelled:
            self._transition(wf, WorkflowStatus.CANCELLED)
            await self._cancel_steps(wf)
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

    @_serialized
    async def claim_step(self, task_id: str):
        """Atomically fence dispatch against workflow pause/cancel/advance."""
        task = await self._tasks.get(task_id)
        if task is None:
            return None
        workflow = await self.get(task.workflow_id)
        if workflow is None or workflow.status != WorkflowStatus.RUNNING:
            return None
        return await self._tasks.transition(task_id, TaskStatus.RUNNING)

    @_serialized
    async def pause(self, workflow_id: str) -> WorkflowRecord:
        wf = await self.get(workflow_id)
        if not wf:
            raise ValueError(f"Workflow '{workflow_id}' not found")
        self._transition(wf, WorkflowStatus.WAITING)
        await self._save(wf)
        return wf

    @_serialized
    async def resume(self, workflow_id: str) -> WorkflowRecord:
        wf = await self.get(workflow_id)
        if not wf:
            raise ValueError(f"Workflow '{workflow_id}' not found")
        self._validate_steps(wf.steps)
        self._transition(wf, WorkflowStatus.RUNNING)
        await self._save(wf)
        return await self._advance(workflow_id)

    @_serialized
    async def cancel(self, workflow_id: str) -> WorkflowRecord:
        wf = await self.get(workflow_id)
        if not wf:
            raise ValueError(f"Workflow '{workflow_id}' not found")
        self._transition(wf, WorkflowStatus.CANCELLED)
        await self._cancel_steps(wf)
        await self._save(wf)
        return wf

    async def _cancel_steps(self, wf: WorkflowRecord) -> None:
        for tid in wf.step_tasks.values():
            t = await self._tasks.get(tid)
            if t and t.status not in TERMINAL_TASK_STATUSES:
                try:
                    await self._tasks.cancel(tid)
                except ValueError:
                    # A step may have become terminal between the status read and
                    # cancel; the workflow itself is already marked cancelled.
                    pass

    async def list_all(self, status: str | None = None) -> list[WorkflowRecord]:
        rows = await self._storage.list_workflows(status=status)
        return [WorkflowRecord.from_dict(r) for r in rows]

    async def _queue_eligible_steps(self, wf: WorkflowRecord) -> None:
        tasks = await self._tasks.list_by_workflow(wf.id)
        task_map = {t.id: t for t in tasks}
        # Recover a crash after task creation but before the workflow map save.
        # Such tasks remain PENDING until the mapping has been durably saved.
        for task in tasks:
            step_id = task.metadata.get("step_id")
            if step_id and step_id not in wf.step_tasks:
                wf.step_tasks[step_id] = task.id
        completed_steps: set[str] = set()
        active_steps: set[str] = set()
        for step in wf.steps:
            tid = wf.step_tasks.get(step.id)
            if tid and tid in task_map:
                t = task_map[tid]
                if t.status == TaskStatus.PENDING:
                    await self._save(wf)
                    t = await self._tasks.transition(t.id, TaskStatus.QUEUED)
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
                metadata={"step_id": step.id, "tool_name": step.tool_name, "tool_params": step.tool_params,
                          "_workflow_timeout_seconds": step.timeout_seconds},
            )
            wf.step_tasks[step.id] = task.id
            wf.current_step = step.id
            await self._save(wf)
            await self._tasks.transition(task.id, TaskStatus.QUEUED)
