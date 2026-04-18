"""Task state machine and task manager."""

from __future__ import annotations

import asyncio
from typing import Any, Awaitable, Callable

from loguru import logger

from echo_agent.tasks.models import TaskRecord, TaskStatus


class TaskStateMachine:
    """Manages state transitions for a single task."""

    _VALID_TRANSITIONS: dict[TaskStatus, set[TaskStatus]] = {
        TaskStatus.PENDING: {TaskStatus.QUEUED, TaskStatus.CANCELLED},
        TaskStatus.QUEUED: {TaskStatus.RUNNING, TaskStatus.CANCELLED},
        TaskStatus.RUNNING: {TaskStatus.SUCCESS, TaskStatus.FAILED, TaskStatus.CANCELLED, TaskStatus.SUSPENDED},
        TaskStatus.FAILED: {TaskStatus.RETRYING, TaskStatus.CANCELLED},
        TaskStatus.RETRYING: {TaskStatus.RUNNING, TaskStatus.CANCELLED},
        TaskStatus.SUSPENDED: {TaskStatus.RUNNING, TaskStatus.CANCELLED},
        TaskStatus.SUCCESS: set(),
        TaskStatus.CANCELLED: set(),
    }

    @staticmethod
    def transition(task: TaskRecord, new_status: TaskStatus) -> bool:
        allowed = TaskStateMachine._VALID_TRANSITIONS.get(task.status, set())
        if new_status not in allowed:
            logger.warning("Invalid transition {} -> {} for task {}", task.status, new_status, task.id)
            return False
        task.status = new_status
        task.updated_at = __import__("datetime").datetime.now().isoformat()
        return True


class TaskManager:
    """Manages a collection of tasks with dependency resolution and execution."""

    def __init__(self):
        self._tasks: dict[str, TaskRecord] = {}
        self._listeners: list[Callable[[TaskRecord, TaskStatus], Awaitable[None]]] = []

    def add_task(self, task: TaskRecord) -> None:
        self._tasks[task.id] = task

    def get_task(self, task_id: str) -> TaskRecord | None:
        return self._tasks.get(task_id)

    def remove_task(self, task_id: str) -> None:
        self._tasks.pop(task_id, None)

    def list_tasks(self, status: TaskStatus | None = None, workflow_id: str | None = None) -> list[TaskRecord]:
        tasks = list(self._tasks.values())
        if status:
            tasks = [t for t in tasks if t.status == status]
        if workflow_id:
            tasks = [t for t in tasks if t.workflow_id == workflow_id]
        return sorted(tasks, key=lambda t: (t.priority, t.created_at))

    def get_ready_tasks(self) -> list[TaskRecord]:
        completed = {t.id for t in self._tasks.values() if t.status == TaskStatus.SUCCESS}
        return [
            t for t in self._tasks.values()
            if t.status in (TaskStatus.PENDING, TaskStatus.QUEUED) and t.can_start(completed)
        ]

    async def transition(self, task_id: str, new_status: TaskStatus) -> bool:
        task = self._tasks.get(task_id)
        if not task:
            return False
        old_status = task.status
        if not TaskStateMachine.transition(task, new_status):
            return False
        for listener in self._listeners:
            try:
                await listener(task, old_status)
            except Exception as e:
                logger.error("Task listener error: {}", e)
        return True

    async def retry_task(self, task_id: str) -> bool:
        task = self._tasks.get(task_id)
        if not task:
            return False
        if task.attempt >= task.max_retries:
            logger.warning("Task {} exceeded max retries ({})", task_id, task.max_retries)
            return False
        task.attempt += 1
        await self.transition(task_id, TaskStatus.RETRYING)
        await self.transition(task_id, TaskStatus.RUNNING)
        return True

    def on_transition(self, listener: Callable[[TaskRecord, TaskStatus], Awaitable[None]]) -> None:
        self._listeners.append(listener)

    def add_subtask(self, parent_id: str, subtask: TaskRecord) -> None:
        parent = self._tasks.get(parent_id)
        if parent:
            subtask.parent_id = parent_id
            subtask.workflow_id = parent.workflow_id
            parent.subtask_ids.append(subtask.id)
        self._tasks[subtask.id] = subtask

    def get_subtasks(self, parent_id: str) -> list[TaskRecord]:
        return [t for t in self._tasks.values() if t.parent_id == parent_id]

    def is_workflow_complete(self, workflow_id: str) -> bool:
        tasks = self.list_tasks(workflow_id=workflow_id)
        return all(t.status in (TaskStatus.SUCCESS, TaskStatus.CANCELLED) for t in tasks)
