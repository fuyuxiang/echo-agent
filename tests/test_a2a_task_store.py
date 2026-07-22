"""Bounded, TTL-based A2A task store: expire terminal tasks, never evict live ones."""

from __future__ import annotations

from echo_agent.a2a.models import A2ATask, TaskState
from echo_agent.a2a.task_store import TaskStore


class _Clock:
    def __init__(self) -> None:
        self.now = 0.0

    def __call__(self) -> float:
        return self.now


def _task(task_id: str, state: TaskState) -> A2ATask:
    return A2ATask(id=task_id, state=state)


def test_terminal_task_expires_after_ttl():
    clock = _Clock()
    store = TaskStore(ttl_seconds=10.0, max_tasks=100, clock=clock)
    store["t1"] = _task("t1", TaskState.COMPLETED)
    assert "t1" in store
    clock.now = 11.0  # past TTL
    assert "t1" not in store
    assert store.get("t1") is None


def test_active_task_never_expires():
    clock = _Clock()
    store = TaskStore(ttl_seconds=10.0, max_tasks=100, clock=clock)
    store["t1"] = _task("t1", TaskState.WORKING)
    clock.now = 10_000.0
    assert "t1" in store  # non-terminal tasks are immune to TTL


def test_capacity_evicts_oldest_terminal_first():
    clock = _Clock()
    store = TaskStore(ttl_seconds=10_000.0, max_tasks=2, clock=clock)
    store["done1"] = _task("done1", TaskState.COMPLETED)
    store["done2"] = _task("done2", TaskState.COMPLETED)
    # Third insert exceeds capacity → oldest terminal (done1) evicted.
    store["done3"] = _task("done3", TaskState.COMPLETED)
    assert "done1" not in store
    assert "done2" in store and "done3" in store


def test_capacity_never_evicts_active_tasks():
    clock = _Clock()
    store = TaskStore(ttl_seconds=10_000.0, max_tasks=2, clock=clock)
    store["live1"] = _task("live1", TaskState.WORKING)
    store["live2"] = _task("live2", TaskState.WORKING)
    # No terminal task to reclaim → active tasks survive even over capacity.
    store["live3"] = _task("live3", TaskState.WORKING)
    assert len(store) == 3
    assert all(k in store for k in ("live1", "live2", "live3"))


def test_state_transition_to_terminal_arms_ttl():
    clock = _Clock()
    store = TaskStore(ttl_seconds=10.0, max_tasks=100, clock=clock)
    task = _task("t1", TaskState.WORKING)
    store["t1"] = task
    clock.now = 100.0
    assert "t1" in store  # still active
    task.state = TaskState.COMPLETED
    store["t1"] = task  # re-store now that it is terminal → TTL armed at now=100
    clock.now = 105.0
    assert "t1" in store
    clock.now = 111.0
    assert "t1" not in store
