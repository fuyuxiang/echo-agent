"""Tests for AgentLoop._record_task_outcome — the safety net that drives a
dispatched board task to a terminal state after its turn finishes.

Covers:
- An incomplete turn (provider error / budget / iteration ceiling / interrupt)
  is written back as FAILED, not SUCCESS.
- A clean turn is written back as SUCCESS (via the REVIEW hop).
- The safety-net terminal transition advances the owning workflow, but only when
  THIS writeback is what closed the task (the agent didn't close it itself).

A lightweight SimpleNamespace stub binds the unbound method, avoiding a full
AgentLoop construction (mirrors test_scheduler_delivery)."""
from types import SimpleNamespace

import pytest

from echo_agent.agent.loop import AgentLoop
from echo_agent.bus.events import ContentBlock, ContentType, EventType, InboundEvent
from echo_agent.tasks.manager import TaskManager
from echo_agent.tasks.models import TaskStatus


class FakeStorage:
    def __init__(self):
        self._tasks: dict[str, dict] = {}

    async def store_task(self, task_id, data):
        self._tasks[task_id] = data

    async def load_task(self, task_id):
        return self._tasks.get(task_id)

    async def list_tasks(self, workflow_id=None, status=None, board_id=None,
                         assignee=None, label=None):
        return list(self._tasks.values())


def _task_event(task_id: str) -> InboundEvent:
    return InboundEvent(
        event_type=EventType.SYSTEM, channel="task", sender_id="dispatcher",
        chat_id=task_id,
        content=[ContentBlock(type=ContentType.TEXT, text="x")],
        metadata={"task_id": task_id},
    )


async def _running_task(manager: TaskManager, **create_kwargs):
    task = await manager.create(title="t", **create_kwargs)
    await manager.transition(task.id, TaskStatus.QUEUED)
    await manager.transition(task.id, TaskStatus.RUNNING)
    return task


@pytest.mark.asyncio
async def test_incomplete_turn_marked_failed_not_success():
    manager = TaskManager(FakeStorage())
    task = await _running_task(manager)
    stub = SimpleNamespace(_task_manager=manager, _workflow_engine=None)
    record = AgentLoop._record_task_outcome.__get__(stub, AgentLoop)

    await record(_task_event(task.id), "incomplete")

    reloaded = await manager.get(task.id)
    assert reloaded.status == TaskStatus.FAILED
    assert reloaded.error  # a reason was recorded


@pytest.mark.asyncio
async def test_completed_turn_marked_success():
    manager = TaskManager(FakeStorage())
    task = await _running_task(manager)
    stub = SimpleNamespace(_task_manager=manager, _workflow_engine=None)
    record = AgentLoop._record_task_outcome.__get__(stub, AgentLoop)

    await record(_task_event(task.id), "completed")

    assert (await manager.get(task.id)).status == TaskStatus.SUCCESS


@pytest.mark.asyncio
async def test_error_turn_marked_failed():
    manager = TaskManager(FakeStorage())
    task = await _running_task(manager)
    stub = SimpleNamespace(_task_manager=manager, _workflow_engine=None)
    record = AgentLoop._record_task_outcome.__get__(stub, AgentLoop)

    await record(_task_event(task.id), "error", "boom")

    reloaded = await manager.get(task.id)
    assert reloaded.status == TaskStatus.FAILED
    assert reloaded.error == "boom"


@pytest.mark.asyncio
async def test_safety_net_advances_workflow_for_step_task():
    """When the agent doesn't close a workflow-step task itself, the writeback
    that drives it terminal must advance the owning workflow so the next steps
    get queued — otherwise the workflow stalls."""
    manager = TaskManager(FakeStorage())
    task = await _running_task(manager, workflow_id="wf_1")
    advanced: list[str] = []

    class _Engine:
        async def on_task_complete(self, task_id):
            advanced.append(task_id)

    stub = SimpleNamespace(_task_manager=manager, _workflow_engine=_Engine())
    record = AgentLoop._record_task_outcome.__get__(stub, AgentLoop)

    await record(_task_event(task.id), "completed")

    assert advanced == [task.id]


@pytest.mark.asyncio
async def test_no_workflow_advance_when_task_already_terminal():
    """If the agent already closed the task (already terminal), the tool already
    advanced the workflow — the safety net must NOT advance again."""
    manager = TaskManager(FakeStorage())
    task = await _running_task(manager, workflow_id="wf_1")
    await manager.transition(task.id, TaskStatus.CANCELLED)  # agent closed it
    advanced: list[str] = []

    class _Engine:
        async def on_task_complete(self, task_id):
            advanced.append(task_id)

    stub = SimpleNamespace(_task_manager=manager, _workflow_engine=_Engine())
    record = AgentLoop._record_task_outcome.__get__(stub, AgentLoop)

    await record(_task_event(task.id), "completed")

    assert advanced == []
    assert (await manager.get(task.id)).status == TaskStatus.CANCELLED


@pytest.mark.asyncio
async def test_no_workflow_advance_for_non_workflow_task():
    manager = TaskManager(FakeStorage())
    task = await _running_task(manager)  # no workflow_id
    advanced: list[str] = []

    class _Engine:
        async def on_task_complete(self, task_id):
            advanced.append(task_id)

    stub = SimpleNamespace(_task_manager=manager, _workflow_engine=_Engine())
    record = AgentLoop._record_task_outcome.__get__(stub, AgentLoop)

    await record(_task_event(task.id), "completed")

    assert advanced == []
