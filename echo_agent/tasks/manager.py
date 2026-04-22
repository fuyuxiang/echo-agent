"""Task manager — CRUD and state-machine transitions backed by SQLite."""

from __future__ import annotations

from typing import Any

from loguru import logger

from echo_agent.tasks.models import (
    TaskRecord,
    TaskStatus,
    VALID_TASK_TRANSITIONS,
    TERMINAL_TASK_STATUSES,
    _now,
)


class TaskManager:
    """Manages task lifecycle with enforced state transitions."""

    def __init__(self, storage: Any):
        self._storage = storage

    async def create(
        self,
        title: str,
        description: str = "",
        workflow_id: str = "",
        parent_task_id: str = "",
        priority: int = 5,
        max_retries: int = 3,
        metadata: dict[str, Any] | None = None,
    ) -> TaskRecord:
        task = TaskRecord(
            title=title, description=description,
            workflow_id=workflow_id, parent_task_id=parent_task_id,
            priority=priority, max_retries=max_retries,
            metadata=metadata or {},
        )
        await self._storage.store_task(task.id, task.to_dict())
        logger.info("Task created: {} '{}'", task.id, title)
        return task

    async def get(self, task_id: str) -> TaskRecord | None:
        data = await self._storage.load_task(task_id)
        if not data:
            return None
        return TaskRecord.from_dict(data)

    async def transition(self, task_id: str, new_status: TaskStatus, **kwargs: Any) -> TaskRecord:
        task = await self.get(task_id)
        if not task:
            raise ValueError(f"Task '{task_id}' not found")
        allowed = VALID_TASK_TRANSITIONS.get(task.status, set())
        if new_status not in allowed:
            raise ValueError(f"Invalid transition: {task.status.value} → {new_status.value}")
        task.status = new_status
        task.updated_at = _now()
        if new_status == TaskStatus.RUNNING and not task.started_at:
            task.started_at = task.updated_at
        if new_status in TERMINAL_TASK_STATUSES:
            task.completed_at = task.updated_at
        if "result" in kwargs:
            task.result = kwargs["result"]
        if "error" in kwargs:
            task.error = kwargs["error"]
        await self._storage.store_task(task.id, task.to_dict())
        logger.info("Task {} → {}", task_id, new_status.value)
        return task

    async def retry(self, task_id: str) -> TaskRecord:
        task = await self.get(task_id)
        if not task:
            raise ValueError(f"Task '{task_id}' not found")
        if task.status != TaskStatus.FAILED:
            raise ValueError(f"Can only retry failed tasks, current: {task.status.value}")
        if task.retry_count >= task.max_retries:
            raise ValueError(f"Max retries ({task.max_retries}) exceeded")
        task.retry_count += 1
        task.error = ""
        task.completed_at = ""
        task.status = TaskStatus.QUEUED
        task.updated_at = _now()
        await self._storage.store_task(task.id, task.to_dict())
        logger.info("Task {} retried (attempt {})", task_id, task.retry_count)
        return task

    async def cancel(self, task_id: str) -> TaskRecord:
        return await self.transition(task_id, TaskStatus.CANCELLED)

    async def update(self, task_id: str, **fields: Any) -> TaskRecord:
        task = await self.get(task_id)
        if not task:
            raise ValueError(f"Task '{task_id}' not found")
        for key in ("title", "description", "priority", "metadata"):
            if key in fields:
                setattr(task, key, fields[key])
        task.updated_at = _now()
        await self._storage.store_task(task.id, task.to_dict())
        return task

    async def list_by_status(self, status: TaskStatus | None = None) -> list[TaskRecord]:
        rows = await self._storage.list_tasks(status=status.value if status else None)
        return [TaskRecord.from_dict(r) for r in rows]

    async def list_by_workflow(self, workflow_id: str) -> list[TaskRecord]:
        rows = await self._storage.list_tasks(workflow_id=workflow_id)
        return [TaskRecord.from_dict(r) for r in rows]
