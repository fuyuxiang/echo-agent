# tests/test_task_dispatcher.py
"""Tests for TaskDispatcher: queued tasks get dispatched to the agent via the bus,
moved to running, and stamped with a running context for later interrupt."""
import asyncio

import pytest

from echo_agent.tasks.dispatcher import TaskDispatcher, render_task_prompt
from echo_agent.tasks.manager import TaskManager
from echo_agent.tasks.models import TaskStatus


class FakeStorage:
    """Minimal in-memory task store so the real TaskManager logic runs."""

    def __init__(self):
        self._tasks: dict[str, dict] = {}

    async def store_task(self, task_id, data):
        self._tasks[task_id] = data

    async def load_task(self, task_id):
        return self._tasks.get(task_id)

    async def list_tasks(self, workflow_id=None, status=None, board_id=None,
                         assignee=None, label=None):
        rows = list(self._tasks.values())
        if status:
            rows = [r for r in rows if r.get("status") == status]
        return rows


class FakeBus:
    def __init__(self, accept=True):
        self.accept = accept
        self.published = []

    async def publish_inbound(self, event):
        self.published.append(event)
        return self.accept


@pytest.mark.asyncio
async def test_dispatch_queued_task_publishes_and_marks_running():
    storage = FakeStorage()
    manager = TaskManager(storage)
    bus = FakeBus()
    task = await manager.create(title="do the thing", description="details")
    await manager.transition(task.id, TaskStatus.QUEUED)

    dispatcher = TaskDispatcher(bus, manager)
    await dispatcher._scan_once()
    await asyncio.sleep(0)  # let the dispatch task run

    assert len(bus.published) == 1
    event = bus.published[0]
    assert event.metadata["task_id"] == task.id
    assert event.session_key == f"task:{task.id}"
    assert event.unattended is True
    assert event.cron_authorized is False  # board tasks must not bypass approval

    reloaded = await manager.get(task.id)
    assert reloaded.status == TaskStatus.RUNNING
    assert reloaded.session_id == f"task:{task.id}"
    assert reloaded.metadata["_interrupt_event_id"] == event.event_id


@pytest.mark.asyncio
async def test_pending_task_is_not_dispatched():
    storage = FakeStorage()
    manager = TaskManager(storage)
    bus = FakeBus()
    await manager.create(title="stays in inbox")  # pending, never queued

    dispatcher = TaskDispatcher(bus, manager)
    await dispatcher._scan_once()
    await asyncio.sleep(0)

    assert bus.published == []


@pytest.mark.asyncio
async def test_running_task_not_redispatched_on_second_scan():
    storage = FakeStorage()
    manager = TaskManager(storage)
    bus = FakeBus()
    task = await manager.create(title="once")
    await manager.transition(task.id, TaskStatus.QUEUED)

    dispatcher = TaskDispatcher(bus, manager)
    await dispatcher._scan_once()
    await asyncio.sleep(0)
    # Now running (not queued) — a second scan finds nothing to dispatch.
    await dispatcher._scan_once()
    await asyncio.sleep(0)

    assert len(bus.published) == 1


@pytest.mark.asyncio
async def test_dispatch_rejected_by_bus_requeues_task():
    """If the bus rejects the publish, the task the dispatcher optimistically
    flipped to RUNNING must roll back to QUEUED so a later scan re-picks it —
    otherwise it is stuck at RUNNING forever with no turn to ever close it."""
    storage = FakeStorage()
    manager = TaskManager(storage)
    bus = FakeBus(accept=False)
    task = await manager.create(title="rejected")
    await manager.transition(task.id, TaskStatus.QUEUED)

    dispatcher = TaskDispatcher(bus, manager)
    await dispatcher._scan_once()
    await asyncio.sleep(0)

    assert len(bus.published) == 1  # attempted once
    reloaded = await manager.get(task.id)
    assert reloaded.status == TaskStatus.QUEUED  # rolled back, not stuck at RUNNING
    assert reloaded.started_at == ""

    # A later scan (bus now healthy) re-picks it and dispatches successfully.
    bus.accept = True
    await dispatcher._scan_once()
    await asyncio.sleep(0)
    assert len(bus.published) == 2
    assert (await manager.get(task.id)).status == TaskStatus.RUNNING


@pytest.mark.asyncio
async def test_requeue_dispatch_failed_noop_when_not_running():
    """Only a RUNNING task (the optimistic pre-publish flip) may roll back. A task
    that a real turn already moved on from must be left alone."""
    storage = FakeStorage()
    manager = TaskManager(storage)
    task = await manager.create(title="t")
    await manager.transition(task.id, TaskStatus.QUEUED)
    await manager.transition(task.id, TaskStatus.RUNNING)
    await manager.transition(task.id, TaskStatus.REVIEW)

    result = await manager.requeue_dispatch_failed(task.id)
    assert result.status == TaskStatus.REVIEW  # untouched


@pytest.mark.asyncio
async def test_render_task_prompt_includes_id_and_title():
    storage = FakeStorage()
    manager = TaskManager(storage)
    task = await manager.create(title="标题", description="描述")
    prompt = render_task_prompt(task)
    assert task.id in prompt
    assert "标题" in prompt
    assert "描述" in prompt


@pytest.mark.asyncio
async def test_mark_terminal_idempotent_on_already_terminal():
    """Writeback must not resurrect a task the agent already cancelled/completed."""
    storage = FakeStorage()
    manager = TaskManager(storage)
    task = await manager.create(title="t")
    await manager.transition(task.id, TaskStatus.QUEUED)
    await manager.transition(task.id, TaskStatus.RUNNING)
    await manager.transition(task.id, TaskStatus.CANCELLED)

    # A late "success" writeback should be a no-op, not raise.
    result = await manager.mark_terminal(task.id, TaskStatus.SUCCESS)
    assert result.status == TaskStatus.CANCELLED


@pytest.mark.asyncio
async def test_mark_terminal_running_to_success_via_review():
    storage = FakeStorage()
    manager = TaskManager(storage)
    task = await manager.create(title="t")
    await manager.transition(task.id, TaskStatus.QUEUED)
    await manager.transition(task.id, TaskStatus.RUNNING)

    result = await manager.mark_terminal(task.id, TaskStatus.SUCCESS, result="done")
    assert result.status == TaskStatus.SUCCESS
    assert result.result == "done"
