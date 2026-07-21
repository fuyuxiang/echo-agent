"""Task manager — CRUD and state-machine transitions backed by SQLite."""

from __future__ import annotations

from typing import Any, Awaitable, Callable

from loguru import logger

from echo_agent.tasks.models import (
    TaskRecord,
    TaskStatus,
    TaskCASConflict,
    VALID_TASK_TRANSITIONS,
    TERMINAL_TASK_STATUSES,
    _now,
)

# An event sink receives (event_type, payload) for every task change so the
# dashboard can push real-time updates. Wired at startup (app.py) to the
# dashboard WS broadcast; None until then (tests, headless runs).
EventSink = Callable[[str, dict[str, Any]], Awaitable[None]]


class TaskManager:
    """Manages task lifecycle with enforced state transitions."""

    def __init__(self, storage: Any):
        self._storage = storage
        self._event_sink: EventSink | None = None

    def set_event_sink(self, sink: EventSink | None) -> None:
        """Wire an async sink that receives every task change (create/transition/
        update). Best-effort: emission never blocks or fails a state change."""
        self._event_sink = sink

    async def _emit(self, event_type: str, task: TaskRecord) -> None:
        """Fire a task event to the sink if one is wired. Swallows everything —
        a broken/slow subscriber must never break the task operation that already
        persisted."""
        sink = self._event_sink
        if sink is None:
            return
        try:
            await sink(event_type, task.to_dict())
        except Exception as e:
            logger.debug("Task event emit ({}) failed: {}", event_type, e)


    async def create(
        self,
        title: str,
        description: str = "",
        workflow_id: str = "",
        parent_task_id: str = "",
        priority: int = 5,
        max_retries: int = 3,
        metadata: dict[str, Any] | None = None,
        labels: list[str] | None = None,
        assignee: str = "",
        source: str = "",
        board_id: str = "default",
        session_id: str = "",
    ) -> TaskRecord:
        task = TaskRecord(
            title=title, description=description,
            workflow_id=workflow_id, parent_task_id=parent_task_id,
            priority=priority, max_retries=max_retries,
            metadata=metadata or {},
            labels=labels or [],
            assignee=assignee,
            source=source,
            board_id=board_id,
            session_id=session_id,
        )
        await self._storage.store_task(task.id, task.to_dict())
        logger.info("Task created: {} '{}'", task.id, title)
        await self._emit("task_created", task)
        return task

    async def get(self, task_id: str) -> TaskRecord | None:
        data = await self._storage.load_task(task_id)
        if not data:
            return None
        return TaskRecord.from_dict(data)

    async def transition(self, task_id: str, new_status: TaskStatus, **kwargs: Any) -> TaskRecord:
        for _ in range(3):
            task = await self.get(task_id)
            if not task:
                raise ValueError(f"Task '{task_id}' not found")
            allowed = VALID_TASK_TRANSITIONS.get(task.status, set())
            if new_status not in allowed:
                raise ValueError(f"Invalid transition: {task.status.value} → {new_status.value}")
            expected_version = task.version
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
            if await self._cas_persist(task, expected_version):
                logger.info("Task {} → {}", task_id, new_status.value)
                await self._emit("task_transitioned", task)
                return task
        raise TaskCASConflict(f"Task '{task_id}' transition to {new_status.value} lost CAS after retries")

    async def _cas_persist(self, task: TaskRecord, expected_version: int) -> bool:
        """Persist a mutated task under optimistic lock. Uses the backend CAS path
        when available (production SQLite), bumping the in-memory version to match
        the row's new value; falls back to a plain store for backends without CAS
        (in-memory test doubles), where there is no concurrency to guard."""
        cas = getattr(self._storage, "cas_store_task", None)
        if cas is None:
            await self._storage.store_task(task.id, task.to_dict())
            return True
        task.version = expected_version + 1
        ok = await cas(task.id, task.to_dict(), expected_version)
        if not ok:
            task.version = expected_version  # roll back optimistic bump before retry
        return ok

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
        await self._emit("task_transitioned", task)
        return task

    async def cancel(self, task_id: str) -> TaskRecord:
        return await self.transition(task_id, TaskStatus.CANCELLED)

    async def set_running_context(
        self, task_id: str, session_key: str, inbound_event_id: str
    ) -> TaskRecord | None:
        """Record which session/turn is executing this task so a later cancel can
        interrupt precisely that turn (not just flip the DB status). Stored on the
        task's session_id + metadata['_interrupt_event_id']. Best-effort: returns
        None if the task vanished, never raises."""
        task = await self.get(task_id)
        if not task:
            return None
        task.session_id = session_key
        task.metadata = {**task.metadata, "_interrupt_event_id": inbound_event_id}
        task.updated_at = _now()
        await self._storage.store_task(task.id, task.to_dict())
        return task

    async def requeue_dispatch_failed(self, task_id: str) -> TaskRecord | None:
        """Roll a task the dispatcher already flipped to RUNNING back to QUEUED
        when the turn never actually started (message bus rejected the publish).
        Without this the task is stuck at RUNNING forever: the dispatcher only
        scans QUEUED, so it would never be re-picked and no turn will ever write
        a terminal state. RUNNING→QUEUED is deliberately NOT a public
        state-machine transition (it would let the board drag running→queued and
        create executor-less "ghost running" tasks); this is the one internal
        path allowed to do it, and only for a task whose turn we know never ran.
        No-op if the task vanished or already left RUNNING (a real turn picked it
        up in the meantime). Never raises."""
        task = await self.get(task_id)
        if not task or task.status != TaskStatus.RUNNING:
            return task
        task.status = TaskStatus.QUEUED
        task.started_at = ""
        task.updated_at = _now()
        await self._storage.store_task(task.id, task.to_dict())
        logger.warning("Task {} re-queued after dispatch was rejected", task_id)
        await self._emit("task_transitioned", task)
        return task

    async def mark_terminal(
        self, task_id: str, status: TaskStatus, *, result: str = "", error: str = ""
    ) -> TaskRecord | None:
        """Idempotently drive a dispatched task to a terminal state after its turn
        finishes. If the agent already completed it via the task tool (now in a
        terminal state), this is a no-op — the writeback must not resurrect or
        double-transition. RUNNING→SUCCESS needs the intermediate REVIEW hop that
        the state machine enforces (mirrors TaskTool.complete)."""
        task = await self.get(task_id)
        if not task or task.status in TERMINAL_TASK_STATUSES:
            return task
        try:
            if status == TaskStatus.SUCCESS and task.status == TaskStatus.RUNNING:
                await self.transition(task_id, TaskStatus.REVIEW)
            return await self.transition(task_id, status, result=result, error=error)
        except ValueError as e:
            logger.debug("Task {} terminal writeback skipped: {}", task_id, e)
            return await self.get(task_id)

    async def update(self, task_id: str, **fields: Any) -> TaskRecord:
        task = await self.get(task_id)
        if not task:
            raise ValueError(f"Task '{task_id}' not found")
        for key in ("title", "description", "priority", "labels", "assignee", "blocked_reason", "review_summary"):
            if key in fields:
                setattr(task, key, fields[key])
        if "metadata" in fields:
            incoming = fields["metadata"]
            if isinstance(incoming, dict):
                task.metadata = {**task.metadata, **incoming}
            else:
                task.metadata = incoming
        task.updated_at = _now()
        await self._storage.store_task(task.id, task.to_dict())
        await self._emit("task_updated", task)
        return task

    async def list_by_status(self, status: TaskStatus | None = None) -> list[TaskRecord]:
        rows = await self._storage.list_tasks(status=status.value if status else None)
        return [TaskRecord.from_dict(r) for r in rows]

    async def list_by_workflow(self, workflow_id: str) -> list[TaskRecord]:
        rows = await self._storage.list_tasks(workflow_id=workflow_id)
        return [TaskRecord.from_dict(r) for r in rows]

    async def list_by_filters(
        self,
        status: str | None = None,
        assignee: str | None = None,
        label: str | None = None,
        board_id: str | None = None,
    ) -> list[TaskRecord]:
        rows = await self._storage.list_tasks(
            status=status, board_id=board_id, assignee=assignee, label=label
        )
        return [TaskRecord.from_dict(r) for r in rows]
