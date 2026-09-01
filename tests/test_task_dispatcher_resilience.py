"""Failure and lifecycle coverage for the board-task dispatcher."""

from __future__ import annotations

import asyncio
from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest

from echo_agent.tasks.dispatcher import TaskDispatcher, new_owner_id, render_task_prompt
from echo_agent.agent.interrupt_manager import InterruptManager
from echo_agent.tasks.models import TaskStatus


class _Bus:
    def __init__(self) -> None:
        self.events: list[object] = []

    async def publish_inbound(self, event: object) -> bool:
        self.events.append(event)
        return True


class _Tasks:
    def __init__(self) -> None:
        self.queued: list[object] = []
        self.current: object | None = None
        self.renew: AsyncMock = AsyncMock(return_value=True)

    async def list_by_status(self, status: TaskStatus) -> list[object]:
        assert status is TaskStatus.QUEUED
        return self.queued

    async def transition(self, task_id: str, status: TaskStatus) -> None:
        assert status is TaskStatus.RUNNING

    async def set_running_context(self, *args: object, **kwargs: object) -> None:
        return None

    async def requeue_dispatch_failed(self, task_id: str) -> None:
        return None

    async def get(self, task_id: str) -> object | None:
        return self.current

    async def renew_lease(self, *args: object) -> bool:
        return await self.renew(*args)


def _task(task_id: str = "t1") -> SimpleNamespace:
    return SimpleNamespace(id=task_id, title="title", description="")


@pytest.mark.asyncio
async def test_start_is_idempotent_and_stop_cancels_both_supervisors() -> None:
    dispatcher = TaskDispatcher(_Bus(), _Tasks(), poll_interval_sec=3600)
    started = asyncio.Event()

    async def forever() -> None:
        started.set()
        await asyncio.Event().wait()

    dispatcher._tick_loop = forever  # type: ignore[method-assign]
    dispatcher._renew_loop = forever  # type: ignore[method-assign]
    await dispatcher.start()
    first = (dispatcher._tick_task, dispatcher._renew_task)
    await started.wait()
    await dispatcher.start()
    assert (dispatcher._tick_task, dispatcher._renew_task) == first

    await dispatcher.stop()
    assert dispatcher._tick_task is None
    assert dispatcher._renew_task is None
    assert all(task is not None and task.cancelled() for task in first)


@pytest.mark.asyncio
async def test_tick_loop_logs_operational_failure_then_continues() -> None:
    dispatcher = TaskDispatcher(_Bus(), _Tasks(), poll_interval_sec=0)

    async def fail_once() -> None:
        dispatcher._running = False
        raise RuntimeError("storage unavailable")

    dispatcher._scan_once = fail_once  # type: ignore[method-assign]
    dispatcher._running = True
    await dispatcher._tick_loop()


@pytest.mark.asyncio
async def test_tick_loop_treats_cancellation_as_shutdown() -> None:
    dispatcher = TaskDispatcher(_Bus(), _Tasks(), poll_interval_sec=0)
    dispatcher._scan_once = AsyncMock(side_effect=asyncio.CancelledError)  # type: ignore[method-assign]
    dispatcher._running = True
    await dispatcher._tick_loop()


@pytest.mark.asyncio
async def test_scan_does_not_duplicate_an_inflight_task() -> None:
    tasks = _Tasks()
    tasks.queued = [_task()]
    dispatcher = TaskDispatcher(_Bus(), tasks)
    dispatcher._inflight.add("t1")
    await dispatcher._scan_once()
    assert dispatcher._dispatch_tasks == set()


@pytest.mark.asyncio
async def test_dispatch_releases_slot_when_renewal_has_lost_the_lease() -> None:
    tasks = _Tasks()
    task = _task()
    tasks.current = SimpleNamespace(
        id=task.id, status=TaskStatus.RUNNING, owner_id="owner"
    )
    dispatcher = TaskDispatcher(
        _Bus(), tasks, owner_id="owner", renew_interval_sec=0.01
    )
    run = asyncio.create_task(dispatcher._dispatch(task))
    while task.id not in dispatcher._pending_release:
        await asyncio.sleep(0)
    dispatcher._stop_renew.add(task.id)
    await asyncio.wait_for(run, timeout=1)
    assert not dispatcher._sem.locked()


@pytest.mark.asyncio
async def test_dispatch_releases_slot_when_task_changes_owner() -> None:
    tasks = _Tasks()
    task = _task()
    tasks.current = SimpleNamespace(
        id=task.id, status=TaskStatus.RUNNING, owner_id="another-instance"
    )
    dispatcher = TaskDispatcher(
        _Bus(), tasks, owner_id="owner", renew_interval_sec=0.01
    )
    await asyncio.wait_for(dispatcher._dispatch(task), timeout=1)
    assert not dispatcher._sem.locked()


@pytest.mark.asyncio
async def test_dispatch_reserves_interrupt_before_exposing_event_to_bus() -> None:
    tasks = _Tasks()
    task = _task()
    tasks.current = SimpleNamespace(
        id=task.id, status=TaskStatus.PENDING, owner_id="owner"
    )
    interrupts = InterruptManager()
    observed_interrupted = False

    class _OvertakingBus:
        async def publish_inbound(self, event: object) -> bool:
            nonlocal observed_interrupted
            assert interrupts.interrupt(event.session_key, event.event_id) is True
            interrupts.request(event.session_key, event.event_id)
            observed_interrupted = interrupts.is_interrupted(event.session_key)
            return True

    dispatcher = TaskDispatcher(
        _OvertakingBus(),
        tasks,
        owner_id="owner",
        renew_interval_sec=0.01,
        interrupt_manager=interrupts,
    )

    await asyncio.wait_for(dispatcher._dispatch(task), timeout=1)

    assert observed_interrupted is True


@pytest.mark.asyncio
async def test_dispatch_rejection_discards_interrupt_admission() -> None:
    tasks = _Tasks()
    task = _task()
    interrupts = InterruptManager()

    class _RejectingBus:
        async def publish_inbound(self, event: object) -> bool:
            assert (event.session_key, event.event_id) in interrupts._admitted
            return False

    dispatcher = TaskDispatcher(
        _RejectingBus(), tasks, interrupt_manager=interrupts,
    )

    await dispatcher._dispatch(task)

    assert not interrupts._admitted


@pytest.mark.asyncio
async def test_renew_loop_marks_a_lease_that_cannot_be_renewed() -> None:
    tasks = _Tasks()
    dispatcher = TaskDispatcher(
        _Bus(), tasks, owner_id="owner", renew_interval_sec=0
    )
    dispatcher._pending_release["t1"] = asyncio.get_running_loop().create_future()

    async def reject(*args: object) -> bool:
        dispatcher._running = False
        return False

    tasks.renew.side_effect = reject
    dispatcher._running = True
    await dispatcher._renew_loop()
    assert "t1" in dispatcher._stop_renew


@pytest.mark.asyncio
async def test_renew_loop_isolates_one_storage_failure() -> None:
    tasks = _Tasks()
    dispatcher = TaskDispatcher(
        _Bus(), tasks, owner_id="owner", renew_interval_sec=0
    )
    dispatcher._pending_release["t1"] = asyncio.get_running_loop().create_future()

    async def fail(*args: object) -> bool:
        dispatcher._running = False
        raise RuntimeError("locked")

    tasks.renew.side_effect = fail
    dispatcher._running = True
    await dispatcher._renew_loop()
    assert "t1" not in dispatcher._stop_renew


def test_helpers_have_stable_public_semantics() -> None:
    assert new_owner_id() != new_owner_id()
    prompt = render_task_prompt(_task())
    assert "描述:" not in prompt
    assert "任务ID: t1" in prompt
