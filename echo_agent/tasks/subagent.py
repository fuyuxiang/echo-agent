"""Sub-agent manager — delegates subtasks to independent execution units."""

from __future__ import annotations

import asyncio
import uuid
from dataclasses import dataclass, field
from typing import Any, Awaitable, Callable

from loguru import logger

from echo_agent.tasks.models import TaskRecord, TaskStatus
from echo_agent.tasks.manager import TaskManager


@dataclass
class SubagentResult:
    task_id: str
    success: bool
    output: str = ""
    error: str = ""
    tools_used: list[str] = field(default_factory=list)


class SubagentManager:
    """Manages sub-agent execution with independent context and parallel support."""

    def __init__(
        self,
        task_manager: TaskManager,
        execute_fn: Callable[[TaskRecord], Awaitable[SubagentResult]],
        max_parallel: int = 5,
    ):
        self._task_manager = task_manager
        self._execute_fn = execute_fn
        self._max_parallel = max_parallel
        self._active: dict[str, asyncio.Task] = {}

    async def dispatch(self, tasks: list[TaskRecord]) -> list[SubagentResult]:
        semaphore = asyncio.Semaphore(self._max_parallel)
        results: list[SubagentResult] = []

        async def _run(task: TaskRecord) -> SubagentResult:
            async with semaphore:
                await self._task_manager.transition(task.id, TaskStatus.RUNNING)
                try:
                    result = await self._execute_fn(task)
                    if result.success:
                        task.result = result.output
                        await self._task_manager.transition(task.id, TaskStatus.SUCCESS)
                    else:
                        task.error = result.error
                        await self._task_manager.transition(task.id, TaskStatus.FAILED)
                    return result
                except Exception as e:
                    task.error = str(e)
                    await self._task_manager.transition(task.id, TaskStatus.FAILED)
                    return SubagentResult(task_id=task.id, success=False, error=str(e))

        for task in tasks:
            await self._task_manager.transition(task.id, TaskStatus.QUEUED)

        batch_results = await asyncio.gather(
            *[_run(t) for t in tasks],
            return_exceptions=True,
        )

        for r in batch_results:
            if isinstance(r, Exception):
                results.append(SubagentResult(task_id="unknown", success=False, error=str(r)))
            else:
                results.append(r)

        return results

    async def dispatch_with_dependencies(self, tasks: list[TaskRecord]) -> list[SubagentResult]:
        """Execute tasks respecting dependency order."""
        for task in tasks:
            self._task_manager.add_task(task)

        all_results: list[SubagentResult] = []
        completed: set[str] = set()

        while True:
            ready = [t for t in tasks if t.id not in completed and t.can_start(completed) and t.status != TaskStatus.SUCCESS]
            if not ready:
                break

            results = await self.dispatch(ready)
            for r in results:
                all_results.append(r)
                if r.success:
                    completed.add(r.task_id)

            if not any(r.success for r in results):
                logger.warning("No tasks succeeded in this round, stopping")
                break

        return all_results

    def get_trace(self, workflow_id: str) -> list[dict[str, Any]]:
        """Get parent-child task chain for observability."""
        tasks = self._task_manager.list_tasks(workflow_id=workflow_id)
        return [t.to_dict() for t in tasks]
