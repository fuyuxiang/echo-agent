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
    ):
        self._bus = bus
        self._tasks = task_manager
        self._poll_interval = poll_interval_sec
        self._running = False
        self._tick_task: asyncio.Task[None] | None = None
        # Tasks already handed off this process — avoids re-dispatching a task
        # that is queued in the DB but whose turn hasn't started/finished yet
        # (the queued→running transition is our real de-dup, but the poll can
        # race ahead of the async turn pickup).
        self._inflight: set[str] = set()
        self._sem = asyncio.Semaphore(max_concurrent)

    async def start(self) -> None:
        if self._running:
            return
        self._running = True
        self._tick_task = asyncio.create_task(self._tick_loop())
        logger.info("Task dispatcher started (poll={}s)", self._poll_interval)

    async def stop(self) -> None:
        self._running = False
        if self._tick_task:
            self._tick_task.cancel()
            try:
                await self._tick_task
            except asyncio.CancelledError:
                pass
            self._tick_task = None
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
            asyncio.create_task(self._dispatch(task))

    async def _dispatch(self, task: Any) -> None:
        async with self._sem:
            try:
                # Move to running BEFORE publishing so a second scan (or another
                # instance) won't pick the same task up again. If the publish is
                # rejected we roll RUNNING back to QUEUED below — otherwise the
                # task would be stuck at RUNNING with no turn to ever close it.
                session_key = f"task:{task.id}"
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
                await self._tasks.transition(task.id, TaskStatus.RUNNING)
                await self._tasks.set_running_context(task.id, session_key, event.event_id)
                accepted = await self._bus.publish_inbound(event)
                if not accepted:
                    # The turn never entered the bus, so no AgentLoop turn will
                    # ever run or write a terminal state for this task. Roll the
                    # optimistic RUNNING back to QUEUED so a later scan re-picks
                    # it instead of leaving it stuck at RUNNING forever.
                    logger.warning("Task {} dispatch rejected by bus (full/stopping), re-queueing", task.id)
                    await self._tasks.requeue_dispatch_failed(task.id)
            except Exception as e:
                logger.error("Failed to dispatch task {}: {}", task.id, e)
            finally:
                # Drop from inflight so a task that finished (or failed to
                # dispatch and got re-queued) can be re-picked on a later scan.
                self._inflight.discard(task.id)
