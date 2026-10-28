"""Real persistence regressions found during the server lifecycle review."""

from __future__ import annotations

import asyncio

import pytest
import pytest_asyncio

from echo_agent.storage.sqlite import SQLiteBackend
from echo_agent.tasks.manager import TaskManager
from echo_agent.tasks.models import TaskStatus, WorkflowStatus
from echo_agent.tasks.workflow import WorkflowEngine


@pytest_asyncio.fixture
async def state(tmp_path):
    storage = SQLiteBackend(tmp_path / "state.sqlite")
    await storage.initialize()
    manager = TaskManager(storage)
    yield storage, manager, WorkflowEngine(storage, manager)
    await storage.close()


@pytest.mark.asyncio
@pytest.mark.parametrize("steps", [
    [],
    [{"id": "same"}, {"id": "same"}],
    [{"id": "a", "depends_on": ["missing"]}],
    [{"id": "a", "depends_on": ["b"]}, {"id": "b", "depends_on": ["a"]}],
    [{"id": "a", "condition": "result == 'ok'"}],
    [{"id": "a", "timeout_seconds": 0}],
    [{"id": "a", "retry_max": -1}],
])
async def test_workflow_rejects_unexecutable_graph_before_persisting(state, steps):
    storage, _, engine = state
    with pytest.raises(ValueError):
        await engine.create("invalid", steps)
    assert await storage.list_workflows() == []


@pytest.mark.asyncio
async def test_cancelled_step_cannot_complete_workflow_successfully(state):
    _, manager, engine = state
    wf = await engine.create("cancel", [{"id": "a"}])
    wf = await engine.start(wf.id)
    await manager.cancel(wf.step_tasks["a"])
    assert (await engine.advance(wf.id)).status == WorkflowStatus.CANCELLED


@pytest.mark.asyncio
async def test_task_tool_cancellation_settles_owning_workflow(state):
    from echo_agent.agent.tools.task import TaskTool

    _, manager, engine = state
    wf = await engine.create("cancel", [{"id": "a"}])
    wf = await engine.start(wf.id)
    result = await TaskTool(manager, workflow_engine=engine).execute(
        {"action": "cancel", "task_id": wf.step_tasks["a"]},
    )
    assert result.success
    assert (await engine.get(wf.id)).status == WorkflowStatus.CANCELLED


@pytest.mark.asyncio
async def test_paused_workflow_does_not_dispatch_dependents(state):
    _, manager, engine = state
    wf = await engine.create("pause", [{"id": "a"}, {"id": "b", "depends_on": ["a"]}])
    wf = await engine.start(wf.id)
    await manager.transition(wf.step_tasks["a"], TaskStatus.RUNNING)
    await manager.mark_terminal(wf.step_tasks["a"], TaskStatus.SUCCESS)
    await engine.pause(wf.id)
    wf = await engine.advance(wf.id)
    assert wf.status == WorkflowStatus.WAITING
    assert "b" not in wf.step_tasks
    assert "b" in (await engine.resume(wf.id)).step_tasks


@pytest.mark.asyncio
async def test_concurrent_workflow_advances_create_one_task_per_step(state):
    _, manager, engine = state
    wf = await engine.create("race", [{"id": "a"}, {"id": "b", "depends_on": ["a"]}])
    wf = await engine.start(wf.id)
    await manager.transition(wf.step_tasks["a"], TaskStatus.RUNNING)
    await manager.mark_terminal(wf.step_tasks["a"], TaskStatus.SUCCESS)
    await asyncio.gather(*(engine.advance(wf.id) for _ in range(10)))
    tasks = await manager.list_by_workflow(wf.id)
    assert [t.metadata["step_id"] for t in tasks].count("b") == 1


@pytest.mark.asyncio
@pytest.mark.parametrize("operation", ["update", "context", "requeue"])
async def test_stale_task_writes_cannot_resurrect_cancelled_state(state, monkeypatch, operation):
    storage, manager, _ = state
    task = await manager.create("race")
    await manager.transition(task.id, TaskStatus.QUEUED)
    await manager.transition(task.id, TaskStatus.RUNNING)
    read, release = asyncio.Event(), asyncio.Event()
    original = storage.load_task

    async def load(task_id):
        result = await original(task_id)
        if asyncio.current_task().get_name() == "stale-writer" and not read.is_set():
            read.set()
            await release.wait()
        return result

    monkeypatch.setattr(storage, "load_task", load)
    if operation == "update":
        pending = manager.update(task.id, title="edited")
    elif operation == "context":
        pending = manager.set_running_context(task.id, "task:old", "old", owner_id="old")
    else:
        pending = manager.requeue_dispatch_failed(task.id)
    writer = asyncio.create_task(pending, name="stale-writer")
    await read.wait()
    cancelled = await manager.cancel(task.id)
    release.set()
    await writer
    final = await manager.get(task.id)
    assert final.status == TaskStatus.CANCELLED
    assert final.version >= cancelled.version
    assert final.completed_at == cancelled.completed_at
    assert final.owner_id == ""
    if operation == "update":
        assert final.title == "edited"


@pytest.mark.asyncio
async def test_concurrent_retries_only_increment_once(state):
    _, manager, _ = state
    task = await manager.create("retry")
    await manager.transition(task.id, TaskStatus.QUEUED)
    await manager.transition(task.id, TaskStatus.RUNNING)
    await manager.mark_terminal(task.id, TaskStatus.FAILED, error="failed")
    results = await asyncio.gather(manager.retry(task.id), manager.retry(task.id), return_exceptions=True)
    assert sum(not isinstance(r, Exception) for r in results) == 1
    assert (await manager.get(task.id)).retry_count == 1


@pytest.mark.asyncio
async def test_generic_failed_to_queued_transition_obeys_retry_budget(state):
    _, manager, _ = state
    task = await manager.create("no retries", max_retries=0)
    await manager.transition(task.id, TaskStatus.QUEUED)
    await manager.transition(task.id, TaskStatus.RUNNING)
    await manager.mark_terminal(task.id, TaskStatus.FAILED, error="original")
    with pytest.raises(ValueError, match="Max retries"):
        await manager.transition(task.id, TaskStatus.QUEUED)
    assert (await manager.get(task.id)).status == TaskStatus.FAILED


@pytest.mark.asyncio
async def test_old_attempt_cannot_finish_retried_task(state):
    _, manager, _ = state
    task = await manager.create("retry")
    await manager.transition(task.id, TaskStatus.QUEUED)
    await manager.transition(task.id, TaskStatus.RUNNING)
    await manager.set_running_context(task.id, "task:retry", "old-event", attempt_id="old")
    await manager.mark_terminal(task.id, TaskStatus.FAILED, error="timeout")
    await manager.retry(task.id)
    await manager.transition(task.id, TaskStatus.RUNNING)
    await manager.set_running_context(task.id, "task:retry", "new-event", attempt_id="new")
    await manager.mark_terminal(task.id, TaskStatus.SUCCESS, result="old result", expected_attempt_id="old")
    final = await manager.get(task.id)
    assert final.status == TaskStatus.RUNNING
    assert final.attempt_id == "new"
    assert final.result == ""


@pytest.mark.asyncio
async def test_workflow_explicit_retry_budget_is_executed(state):
    _, manager, engine = state
    wf = await engine.create("retry", [{"id": "a", "retry_max": 1}])
    wf = await engine.start(wf.id)
    task_id = wf.step_tasks["a"]
    await manager.transition(task_id, TaskStatus.RUNNING)
    await manager.mark_terminal(task_id, TaskStatus.FAILED)
    assert (await engine.advance(wf.id)).status == WorkflowStatus.RUNNING
    assert (await manager.get(task_id)).retry_count == 1
    await manager.transition(task_id, TaskStatus.RUNNING)
    await manager.mark_terminal(task_id, TaskStatus.FAILED)
    assert (await engine.advance(wf.id)).status == WorkflowStatus.FAILED


@pytest.mark.asyncio
async def test_step_creation_recovers_before_workflow_map_was_saved(state, monkeypatch):
    storage, manager, engine = state
    wf = await engine.create("crash", [{"id": "a"}])
    save = storage.store_workflow
    fail = True

    async def fail_once(*args):
        nonlocal fail
        if fail:
            fail = False
            raise OSError("simulated disk failure after task creation")
        return await save(*args)

    monkeypatch.setattr(storage, "store_workflow", fail_once)
    with pytest.raises(OSError):
        await engine.start(wf.id)
    orphan = (await manager.list_by_workflow(wf.id))[0]
    assert orphan.status == TaskStatus.PENDING
    restarted_engine = WorkflowEngine(storage, manager)
    resumed = await restarted_engine.start(wf.id)
    assert resumed.step_tasks == {"a": orphan.id}
    assert len(await manager.list_by_workflow(wf.id)) == 1
    assert (await manager.get(orphan.id)).status == TaskStatus.QUEUED


@pytest.mark.asyncio
async def test_workflow_timeout_interrupts_exact_turn_and_fails_workflow(state):
    from echo_agent.agent.interrupt_manager import InterruptManager
    from echo_agent.bus.queue import MessageBus
    from echo_agent.tasks.dispatcher import TaskDispatcher

    _, manager, engine = state
    interrupts, bus = InterruptManager(), MessageBus()
    dispatcher = TaskDispatcher(bus, manager, workflow_engine=engine, interrupt_manager=interrupts,
                                owner_id="runtime", renew_interval_sec=0.05)
    manager.add_terminal_listener(dispatcher._on_task_terminal)
    wf = await engine.create("timeout", [{"id": "a", "timeout_seconds": 1}])
    wf = await engine.start(wf.id)
    task = await manager.get(wf.step_tasks["a"])
    await asyncio.wait_for(dispatcher._dispatch(task), timeout=3)
    final = await manager.get(task.id)
    assert final.status == TaskStatus.FAILED
    assert "timed out" in final.error
    assert (await engine.get(wf.id)).status == WorkflowStatus.FAILED
    event = await bus._inbound_queue.get()
    interrupts.request(event.session_key, event.event_id)
    assert interrupts.is_interrupted(event.session_key)


@pytest.mark.asyncio
async def test_paused_workflow_queued_task_is_not_dispatched(state):
    from echo_agent.bus.queue import MessageBus
    from echo_agent.tasks.dispatcher import TaskDispatcher

    _, manager, engine = state
    bus = MessageBus()
    dispatcher = TaskDispatcher(bus, manager, workflow_engine=engine)
    wf = await engine.create("pause", [{"id": "a"}])
    wf = await engine.start(wf.id)
    await engine.pause(wf.id)
    await dispatcher._dispatch(await manager.get(wf.step_tasks["a"]))
    assert bus.pending_inbound == 0
    assert (await manager.get(wf.step_tasks["a"])).status == TaskStatus.QUEUED


@pytest.mark.asyncio
async def test_old_task_tool_call_cannot_complete_new_attempt(state):
    from echo_agent.agent.tools.task import TaskTool
    from echo_agent.tools import ToolExecutionContext

    _, manager, _ = state
    task = await manager.create("new attempt")
    await manager.transition(task.id, TaskStatus.QUEUED)
    await manager.transition(task.id, TaskStatus.RUNNING)
    await manager.set_running_context(task.id, f"task:{task.id}", "new-event", attempt_id="new")
    result = await TaskTool(manager).execute(
        {"action": "complete", "task_id": task.id},
        ToolExecutionContext(session_key=f"task:{task.id}", inbound_event_id="old-event"),
    )
    assert not result.success
    assert (await manager.get(task.id)).status == TaskStatus.RUNNING
