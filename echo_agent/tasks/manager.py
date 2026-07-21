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
    _now_ms,
)

# An event sink receives (event_type, payload) for every task change so the
# dashboard can push real-time updates. Wired at startup (app.py) to the
# dashboard WS broadcast; None until then (tests, headless runs).
EventSink = Callable[[str, dict[str, Any]], Awaitable[None]]

# A terminal listener is fired once a task reaches SUCCESS/FAILED/CANCELLED so
# the dispatcher can release the concurrency semaphore only after the whole turn
# (not merely the publish) is done — see decision (d).
TerminalListener = Callable[[str, "TaskStatus"], Awaitable[None]]


class TaskManager:
    """Manages task lifecycle with enforced state transitions."""

    def __init__(self, storage: Any):
        self._storage = storage
        self._event_sink: EventSink | None = None
        self._terminal_listeners: list[TerminalListener] = []

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
                if new_status in TERMINAL_TASK_STATUSES:
                    await self._fire_terminal(task_id, new_status)
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
        self, task_id: str, session_key: str, inbound_event_id: str,
        *, owner_id: str = "", lease_ttl_ms: int = 0, attempt_id: str = "",
    ) -> TaskRecord | None:
        """Record which session/turn is executing this task so a later cancel can
        interrupt precisely that turn, and (when the dispatcher provides them)
        stamp the lease trio owner_id/attempt_id/lease_until_ms so a crashed
        instance's RUNNING tasks can be reclaimed by whoever holds the workspace
        next. Best-effort: returns None if the task vanished, never raises.
        lease_ttl_ms == 0 keeps the old behaviour (no lease); a negative TTL
        stamps an already-expired lease (used to simulate a stale lease)."""
        task = await self.get(task_id)
        if not task:
            return None
        task.session_id = session_key
        task.metadata = {**task.metadata, "_interrupt_event_id": inbound_event_id}
        if owner_id:
            task.owner_id = owner_id
        if attempt_id:
            task.attempt_id = attempt_id
        if lease_ttl_ms != 0:
            task.lease_until_ms = _now_ms() + lease_ttl_ms
        task.updated_at = _now()
        await self._storage.store_task(task.id, task.to_dict())
        return task

    async def renew_lease(self, task_id: str, owner_id: str, lease_ttl_ms: int) -> bool:
        """Extend a RUNNING task's lease if we still own it. Returns False when the
        task left RUNNING or another owner took it — the caller must stop renewing."""
        task = await self.get(task_id)
        if not task or task.status != TaskStatus.RUNNING or task.owner_id != owner_id:
            return False
        expected_version = task.version
        task.lease_until_ms = _now_ms() + lease_ttl_ms
        task.updated_at = _now()
        # Route through CAS so the lease write only lands if the version still
        # matches. A lost CAS means a concurrent writer advanced the task (e.g. a
        # terminal transition) — return False so the renewer stops, and never
        # revert the terminal state back to RUNNING.
        return await self._cas_persist(task, expected_version)

    async def reclaim_expired_running(
        self, *, current_owner_id: str, now_ms: int | None = None
    ) -> list[str]:
        """Requeue RUNNING tasks whose executor is gone: either stamped by a
        previous instance (owner_id != current) or whose lease has expired. Called
        once at startup so a crash mid-turn no longer strands a task at RUNNING
        forever. Returns the reclaimed task ids."""
        # NOTE: assumes a single active instance per workspace (enforced by the
        # runtime instance-lock). requeue here is a plain write, not CAS; that is
        # safe only because no concurrent instance shares this DB. If multi-instance
        # shared-DB is ever supported, requeue_dispatch_failed must become CAS-guarded
        # to avoid clobbering a peer's concurrent terminal write.
        ref = now_ms if now_ms is not None else _now_ms()
        reclaimed: list[str] = []
        for task in await self.list_by_status(TaskStatus.RUNNING):
            foreign = bool(task.owner_id) and task.owner_id != current_owner_id
            expired = task.lease_until_ms is not None and task.lease_until_ms < ref
            if foreign or expired:
                rolled = await self.requeue_dispatch_failed(task.id)
                if rolled is not None and rolled.status == TaskStatus.QUEUED:
                    reclaimed.append(task.id)
        return reclaimed

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

    def add_terminal_listener(self, listener: TerminalListener) -> None:
        """Register an async callback fired after a task reaches a terminal state.
        The dispatcher uses this to release the concurrency semaphore only once the
        whole turn (not merely the publish) is done — see decision (d)."""
        self._terminal_listeners.append(listener)

    async def _fire_terminal(self, task_id: str, status: TaskStatus) -> None:
        """Notify terminal listeners. Best-effort: a broken listener must never
        fail the state change that already persisted."""
        for listener in list(self._terminal_listeners):
            try:
                await listener(task_id, status)
            except Exception as e:
                logger.debug("Terminal listener failed for task {}: {}", task_id, e)

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
