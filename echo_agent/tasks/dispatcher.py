"""Task dispatcher — feeds QUEUED board tasks to the agent for execution.

The task subsystem has no executor of its own: TaskManager/WorkflowEngine only
track state, and the agent (LLM) is the executor. This dispatcher is the missing
bridge — it polls for QUEUED tasks and hands each one to the agent as an inbound
event, mirroring how the scheduler turns a cron job into an InboundEvent on the
bus (see scheduler/service.py + scheduler/delivery.py).

Design decisions (confirmed with product owner):
- Auto-dispatch: queued tasks run without human action.
- Isolated session per task ("task:{id}") so a task run never mixes with a
  human's chat history, and cancel-interrupt can never clip an unrelated turn.
- Terminal writeback happens in AgentLoop after the turn (see
  _record_task_outcome), not here — this class only starts work.
"""

from __future__ import annotations

import asyncio
import uuid
from typing import Any

from loguru import logger

from echo_agent.bus.events import ContentBlock, ContentType, EventType, InboundEvent
from echo_agent.tasks.models import TaskStatus


def render_task_prompt(task: Any) -> str:
    """Render a task record into the natural-language instruction the agent
    receives. Kept deliberately explicit about the expected close-out so the
    agent drives the task tool; the loop still writes back a terminal state as a
    safety net if it doesn't."""
    lines = [
        "你有一个待执行的任务,请开始处理并在完成后给出结果。",
        f"任务ID: {task.id}",
        f"标题: {task.title}",
    ]
    if task.description:
        lines.append(f"描述: {task.description}")
    lines.append(
        "完成后请用 task 工具将其标记为 complete(附结果摘要);若无法完成,"
        "标记为 fail(附原因)。"
    )
    return "\n".join(lines)


class TaskDispatcher:
    """Polls QUEUED tasks and dispatches them to the agent via the message bus."""

    def __init__(
        self,
        bus: Any,
        task_manager: Any,
        *,
        poll_interval_sec: float = 3.0,
        max_concurrent: int = 3,
        owner_id: str = "",
        lease_ttl_ms: int = 60000,
        renew_interval_sec: float = 20.0,
        stop_grace_sec: float = 30.0,
    ):
        self._bus = bus
        self._tasks = task_manager
        self._poll_interval = poll_interval_sec
        self._owner_id = owner_id
        self._lease_ttl_ms = lease_ttl_ms
        self._renew_interval = renew_interval_sec
        self._stop_grace = stop_grace_sec
        self._running = False
        self._tick_task: asyncio.Task[None] | None = None
        self._renew_task: asyncio.Task[None] | None = None
        self._inflight: set[str] = set()
        # Strong refs to in-flight dispatch coroutines: without this,
        # asyncio.create_task returns a task the event loop only weakly
        # references, so a slow turn's dispatch can be GC'd mid-flight.
        self._dispatch_tasks: set[asyncio.Task[None]] = set()
        # Per-task future resolved when the task hits a terminal state; the
        # dispatch coroutine awaits it before releasing the concurrency slot so
        # one slot covers the WHOLE turn, not just the publish (decision d).
        self._pending_release: dict[str, asyncio.Future[None]] = {}
        self._sem = asyncio.Semaphore(max_concurrent)

    async def start(self) -> None:
        if self._running:
            return
        self._running = True
        self._tick_task = asyncio.create_task(self._tick_loop())
        self._renew_task = asyncio.create_task(self._renew_loop())
        logger.info("Task dispatcher started (poll={}s)", self._poll_interval)

    async def stop(self) -> None:
        self._running = False
        for t in (self._tick_task, self._renew_task):
            if t:
                t.cancel()
                try:
                    await t
                except asyncio.CancelledError:
                    pass
        self._tick_task = None
        self._renew_task = None
        # Wait for in-flight dispatches to finish their turn, but bounded: a turn
        # that never reaches terminal must not hang shutdown, so cancel on grace
        # timeout (the task stays RUNNING in the DB and a later instance reclaims
        # it via the lease).
        if self._dispatch_tasks:
            pending = list(self._dispatch_tasks)
            try:
                await asyncio.wait_for(
                    asyncio.gather(*pending, return_exceptions=True), timeout=self._stop_grace
                )
            except asyncio.TimeoutError:
                for t in pending:
                    t.cancel()
                await asyncio.gather(*pending, return_exceptions=True)
        logger.info("Task dispatcher stopped")

    async def _tick_loop(self) -> None:
        while self._running:
            try:
                await self._scan_once()
            except asyncio.CancelledError:
                break
            except Exception as e:
                logger.error("Task dispatcher tick error: {}", e)
            await asyncio.sleep(self._poll_interval)

    async def _scan_once(self) -> None:
        tasks = await self._tasks.list_by_status(TaskStatus.QUEUED)
        for task in tasks:
            if task.id in self._inflight:
                continue
            self._inflight.add(task.id)
            dtask = asyncio.create_task(self._dispatch(task))
            self._dispatch_tasks.add(dtask)
            dtask.add_done_callback(self._dispatch_tasks.discard)

    async def _dispatch(self, task: Any) -> None:
        await self._sem.acquire()
        released = False
        try:
            session_key = f"task:{task.id}"
            attempt_id = uuid.uuid4().hex
            event = InboundEvent(
                event_type=EventType.SYSTEM,
                channel="task",
                sender_id="dispatcher",
                chat_id=task.id,
                content=[ContentBlock(type=ContentType.TEXT, text=render_task_prompt(task))],
                session_key_override=session_key,
                # No human at the keyboard. Deliberately NOT cron_authorized:
                # board tasks must not silently bypass EXEC/dangerous-tool
                # approval the way an up-front-approved cron job does.
                unattended=True,
                metadata={"task_id": task.id},
            )
            fut: asyncio.Future[None] = asyncio.get_event_loop().create_future()
            self._pending_release[task.id] = fut
            try:
                # Move to running BEFORE publishing so a second scan (or another
                # instance) won't pick the same task up again.
                await self._tasks.transition(task.id, TaskStatus.RUNNING)
                await self._tasks.set_running_context(
                    task.id, session_key, event.event_id,
                    owner_id=self._owner_id, lease_ttl_ms=self._lease_ttl_ms,
                    attempt_id=attempt_id,
                )
                accepted = await self._bus.publish_inbound(event)
            except Exception as e:
                # 缺口(a):transition/context/publish 任一步异常都回队,
                # 否则任务卡在 RUNNING 无 turn 收尾。
                logger.error("Failed to dispatch task {}: {}", task.id, e)
                await self._tasks.requeue_dispatch_failed(task.id)
                return
            if not accepted:
                logger.warning("Task {} dispatch rejected by bus (full/stopping), re-queueing", task.id)
                await self._tasks.requeue_dispatch_failed(task.id)
                return
            # 缺口(d):publish 成功,持槽等到 turn 终态(或租约超时兜底)才释放。
            try:
                await asyncio.wait_for(fut, timeout=self._lease_ttl_ms / 1000)
            except asyncio.TimeoutError:
                logger.warning("Task {} turn did not reach terminal within lease, releasing slot", task.id)
        finally:
            self._pending_release.pop(task.id, None)
            self._inflight.discard(task.id)
            if not released:
                self._sem.release()

    async def _on_task_terminal(self, task_id: str, status: Any) -> None:
        """Terminal listener (registered on TaskManager): unblock the dispatch
        coroutine holding this task's slot so it releases the semaphore. Async
        (matches TaskManager._fire_terminal's ``await listener(...)``) and
        exception-free so a manager _fire_terminal never breaks on us."""
        fut = self._pending_release.get(task_id)
        if fut is not None and not fut.done():
            fut.set_result(None)

    async def _renew_loop(self) -> None:
        """Periodically extend the lease of tasks whose turn is still in flight so
        another instance's reclaim scan doesn't steal a task that is actively
        running here."""
        while self._running:
            for task_id in list(self._pending_release.keys()):
                try:
                    await self._tasks.renew_lease(task_id, self._owner_id, self._lease_ttl_ms)
                except Exception as e:
                    logger.debug("Lease renew failed for task {}: {}", task_id, e)
            await asyncio.sleep(self._renew_interval)
