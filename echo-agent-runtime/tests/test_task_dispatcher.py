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


@pytest.mark.asyncio
async def test_event_sink_receives_task_changes():
    """Every task change emits a typed event to the wired sink so the dashboard
    can push real-time updates. create → task_created; transitions/retry/requeue
    → task_transitioned; update → task_updated."""
    storage = FakeStorage()
    manager = TaskManager(storage)
    events: list[tuple[str, str]] = []

    async def sink(event_type, payload):
        events.append((event_type, payload["status"]))

    manager.set_event_sink(sink)

    task = await manager.create(title="t")
    await manager.transition(task.id, TaskStatus.QUEUED)
    await manager.transition(task.id, TaskStatus.RUNNING)
    await manager.update(task.id, title="renamed")
    await manager.mark_terminal(task.id, TaskStatus.FAILED, error="x")

    assert ("task_created", "pending") in events
    assert ("task_transitioned", "queued") in events
    assert ("task_transitioned", "running") in events
    assert ("task_updated", "running") in events
    assert ("task_transitioned", "failed") in events


@pytest.mark.asyncio
async def test_event_sink_failure_does_not_break_operation():
    """A broken/slow subscriber must never fail the task operation that already
    persisted."""
    storage = FakeStorage()
    manager = TaskManager(storage)

    async def bad_sink(event_type, payload):
        raise RuntimeError("subscriber down")

    manager.set_event_sink(bad_sink)

    task = await manager.create(title="t")  # must not raise
    assert (await manager.get(task.id)).status == TaskStatus.PENDING


class _FakeStorage:
    def __init__(self):
        self._tasks: dict[str, dict] = {}

    async def store_task(self, task_id, data):
        self._tasks[task_id] = data

    async def load_task(self, task_id):
        return self._tasks.get(task_id)

    async def list_tasks(self, workflow_id=None, status=None, board_id=None, assignee=None, label=None):
        rows = list(self._tasks.values())
        if status:
            rows = [r for r in rows if r.get("status") == status]
        return rows


class _FakeBus:
    def __init__(self, accept=True):
        self.accept = accept
        self.published = []

    async def publish_inbound(self, event):
        self.published.append(event)
        return self.accept


@pytest.mark.asyncio
async def test_exception_between_transition_and_publish_requeues():
    """缺口(a):不止 publish 拒绝,transition 后 publish 前任何异常都要回 QUEUED。"""
    storage = _FakeStorage()
    manager = TaskManager(storage)
    bus = _FakeBus()
    task = await manager.create(title="boom")
    await manager.transition(task.id, TaskStatus.QUEUED)

    async def _raise(event):
        raise RuntimeError("publish blew up")

    bus.publish_inbound = _raise
    dispatcher = TaskDispatcher(bus, manager, owner_id="inst-1")
    await dispatcher._dispatch(task)

    assert (await manager.get(task.id)).status == TaskStatus.QUEUED


@pytest.mark.asyncio
async def test_semaphore_released_only_after_terminal():
    """缺口(d):publish 成功后信号量不释放,直到任务进终态。"""
    storage = _FakeStorage()
    manager = TaskManager(storage)
    bus = _FakeBus()
    task = await manager.create(title="holds slot")
    await manager.transition(task.id, TaskStatus.QUEUED)

    dispatcher = TaskDispatcher(bus, manager, owner_id="inst-1", max_concurrent=1, lease_ttl_ms=60000)
    manager.add_terminal_listener(dispatcher._on_task_terminal)

    d_task = asyncio.create_task(dispatcher._dispatch(task))
    await asyncio.sleep(0.05)
    # 已 publish 但信号量仍被占:另一个 acquire 拿不到。
    assert dispatcher._sem.locked()
    assert len(bus.published) == 1

    # 任务进终态后,信号量应被释放,_dispatch 收尾。
    # (_dispatch 已把任务置 RUNNING,这里直接推入终态即可。)
    await manager.transition(task.id, TaskStatus.CANCELLED)
    await asyncio.wait_for(d_task, timeout=1.0)
    assert not dispatcher._sem.locked()


@pytest.mark.asyncio
async def test_long_turn_past_lease_does_not_release_slot_early():
    """缺口(d) 回归:回合运行时间超过 lease_ttl_ms,但任务仍是本实例持有的
    RUNNING,信号量不得提前释放——旧实现固定 lease_ttl_ms 到点放槽,会令并发
    静默超过 max_concurrent。"""
    storage = _FakeStorage()
    manager = TaskManager(storage)
    bus = _FakeBus()
    task = await manager.create(title="long turn")
    await manager.transition(task.id, TaskStatus.QUEUED)

    # Short lease + short poll cadence so the test crosses several renew
    # intervals AND the old fixed lease_ttl_ms timeout within a fraction of a
    # second.
    dispatcher = TaskDispatcher(
        bus, manager, owner_id="inst-1", max_concurrent=1,
        lease_ttl_ms=100, renew_interval_sec=0.05,
    )
    manager.add_terminal_listener(dispatcher._on_task_terminal)

    d_task = asyncio.create_task(dispatcher._dispatch(task))
    # Wait well past lease_ttl_ms/1000 (0.1s) and multiple renew intervals.
    await asyncio.sleep(0.35)
    # Task never left RUNNING and is still ours → slot MUST still be held.
    assert dispatcher._sem.locked()
    assert (await manager.get(task.id)).status == TaskStatus.RUNNING

    # Reaching terminal finally releases the slot and lets _dispatch return.
    await manager.transition(task.id, TaskStatus.CANCELLED)
    await asyncio.wait_for(d_task, timeout=1.0)
    assert not dispatcher._sem.locked()


@pytest.mark.asyncio
async def test_stop_waits_for_inflight_then_times_out():
    """缺口(b)(c):在途 dispatch task 被强引用,stop 带超时等待,超时 cancel。"""
    storage = _FakeStorage()
    manager = TaskManager(storage)
    bus = _FakeBus()
    task = await manager.create(title="inflight")
    await manager.transition(task.id, TaskStatus.QUEUED)

    dispatcher = TaskDispatcher(bus, manager, owner_id="inst-1", max_concurrent=1,
                                lease_ttl_ms=60000, stop_grace_sec=0.2)
    manager.add_terminal_listener(dispatcher._on_task_terminal)
    await dispatcher._scan_once()
    await asyncio.sleep(0.05)
    assert dispatcher._dispatch_tasks  # 被强引用,未被 GC

    # 任务永不进终态 → stop 等 grace 后 cancel 在途,不挂死。
    await dispatcher.stop()
    assert all(t.done() for t in dispatcher._dispatch_tasks) or not dispatcher._dispatch_tasks
