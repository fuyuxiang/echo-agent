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


def test_active_task_immune_to_terminal_ttl():
    clock = _Clock()
    store = TaskStore(ttl_seconds=10.0, max_tasks=100, active_ttl_seconds=1e6, clock=clock)
    store["t1"] = _task("t1", TaskState.WORKING)
    clock.now = 10_000.0
    assert "t1" in store  # non-terminal tasks are immune to the terminal TTL


def test_stuck_active_task_reclaimed_by_backstop():
    # 活跃任务免疫终态 TTL,但不能永生:卡在 WORKING 从不转终态的任务
    # 既躲过 TTL 又躲过容量淘汰,会无上界泄漏并阻塞其后所有淘汰。
    clock = _Clock()
    store = TaskStore(ttl_seconds=10.0, max_tasks=100, active_ttl_seconds=100.0, clock=clock)
    store["stuck"] = _task("stuck", TaskState.WORKING)
    clock.now = 99.0
    assert "stuck" in store
    clock.now = 101.0
    assert "stuck" not in store


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
    # 宁可暂时超容量也不丢在飞任务(丢了会让调用方永远等不到结果)。
    # 这只是"暂时":卡死不转终态的任务由 active_ttl 兜底回收,见
    # test_stuck_active_task_reclaimed_by_backstop。
    clock = _Clock()
    store = TaskStore(ttl_seconds=10_000.0, max_tasks=2, active_ttl_seconds=1e6, clock=clock)
    store["live1"] = _task("live1", TaskState.WORKING)
    store["live2"] = _task("live2", TaskState.WORKING)
    store["live3"] = _task("live3", TaskState.WORKING)
    assert len(store) == 3
    assert all(k in store for k in ("live1", "live2", "live3"))


def test_non_positive_bounds_rejected():
    # 旧行为:ttl/容量配成 0 或负数时终态任务写入后立刻不可见,
    # tasks/send 报成功但 tasks/get 永远 not found —— 静默失效,改为快速失败。
    import pytest
    for kwargs in (
        {"ttl_seconds": 0}, {"ttl_seconds": -1},
        {"max_tasks": 0}, {"max_tasks": -5},
        {"active_ttl_seconds": 0}, {"active_ttl_seconds": -1},
    ):
        with pytest.raises(ValueError):
            TaskStore(**kwargs)


def test_bookkeeping_stays_in_lockstep():
    # 三本账(_tasks/_expire_at/_stored_at)必须同步增删,否则任一本泄漏
    # 都会让条目永不回收或让淘汰逻辑误判活跃。
    clock = _Clock()
    store = TaskStore(ttl_seconds=10.0, max_tasks=2, clock=clock)
    store["a"] = _task("a", TaskState.COMPLETED)
    store["b"] = _task("b", TaskState.WORKING)
    store["c"] = _task("c", TaskState.COMPLETED)
    store["d"] = _task("d", TaskState.COMPLETED)  # 触发容量淘汰
    clock.now = 50.0
    len(store)  # 触发 _purge_expired
    assert set(store._expire_at) <= set(store._tasks)
    assert set(store._stored_at) == set(store._tasks)


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
