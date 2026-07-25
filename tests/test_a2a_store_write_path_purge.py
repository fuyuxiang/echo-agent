"""纯写入流量下,过期任务也必须被回收。

原实现只在读路径(__getitem__/__contains__/__len__/get)调 _purge_expired,写路径
不清理;容量淘汰又只认已 arm TTL 的终态条目。于是"只有匿名卡住任务持续写入、没人
读"的流量里,即使每条都远超 active TTL,底层表仍会越过 max_tasks 一直长,直到某次
读取才被清掉。
"""

from __future__ import annotations

import pytest

from echo_agent.a2a.models import A2ATask, TaskState
from echo_agent.a2a.task_store import TaskStore


class _Clock:
    def __init__(self) -> None:
        self.now = 0.0

    def __call__(self) -> float:
        return self.now


def _raw_size(store: TaskStore) -> int:
    """底层表大小,绕开 len() —— len() 自己会 purge,会掩盖写路径的缺陷。"""
    return len(store._tasks)


def test_write_path_purges_expired_active_tasks():
    clock = _Clock()
    store = TaskStore(ttl_seconds=10.0, max_tasks=2, active_ttl_seconds=100.0, clock=clock)

    # 三个永不终结的任务(模拟卡住的匿名任务),彼此间隔足够长。
    for i in range(3):
        clock.now = i * 1.0
        store[f"w{i}"] = A2ATask(id=f"w{i}", state=TaskState.WORKING)
    assert _raw_size(store) == 3, "活跃任务不参与容量淘汰,这里本就会超过 max_tasks"

    # 越过 active 兜底后继续只写不读:新写入必须顺手把过期的清掉。
    clock.now = 500.0
    store["w9"] = A2ATask(id="w9", state=TaskState.WORKING)

    assert _raw_size(store) == 1, (
        f"写路径未清理过期条目,底层表仍有 {_raw_size(store)} 条"
    )
    assert "w9" in store._tasks, "刚写入的任务必须保留"


def test_write_path_purges_expired_terminal_tasks():
    clock = _Clock()
    store = TaskStore(ttl_seconds=10.0, max_tasks=100, active_ttl_seconds=1e9, clock=clock)
    for i in range(5):
        store[f"t{i}"] = A2ATask(id=f"t{i}", state=TaskState.COMPLETED)
    assert _raw_size(store) == 5

    clock.now = 50.0  # 全部越过终态 TTL
    store["fresh"] = A2ATask(id="fresh", state=TaskState.COMPLETED)

    assert _raw_size(store) == 1, "过期终态条目必须在写入时被回收"
    assert "fresh" in store._tasks


def test_capacity_eviction_prefers_expired_over_live():
    """先清过期、再谈淘汰:有过期条目可回收时,不该淘汰仍在 TTL 内的结果。"""
    clock = _Clock()
    store = TaskStore(ttl_seconds=10.0, max_tasks=2, active_ttl_seconds=1e9, clock=clock)
    store["old"] = A2ATask(id="old", state=TaskState.COMPLETED)  # t=0,到 t=10 过期

    clock.now = 20.0
    store["recent"] = A2ATask(id="recent", state=TaskState.COMPLETED)  # 到 t=30 过期
    store["newest"] = A2ATask(id="newest", state=TaskState.COMPLETED)

    assert "old" not in store._tasks, "过期条目应先被回收"
    assert "recent" in store._tasks, "仍在 TTL 内的结果不该被淘汰"
    assert "newest" in store._tasks


def test_on_drop_fires_for_every_reclaim_path():
    """回收回调必须覆盖 TTL 过期与容量淘汰两条路径。

    protocol 用它清理自己的旁表;漏掉任一条路径,旁表就会变成新的无上界增长点。
    """
    clock = _Clock()
    dropped: list[str] = []
    store = TaskStore(
        ttl_seconds=10.0, max_tasks=1, active_ttl_seconds=1e9,
        clock=clock, on_drop=dropped.append,
    )

    # 容量淘汰路径。
    store["a"] = A2ATask(id="a", state=TaskState.COMPLETED)
    store["b"] = A2ATask(id="b", state=TaskState.COMPLETED)
    assert dropped == ["a"], f"容量淘汰未触发回调: {dropped}"

    # TTL 过期路径。
    clock.now = 100.0
    assert store.get("b") is None
    assert "b" in dropped, f"TTL 回收未触发回调: {dropped}"


@pytest.mark.asyncio
async def test_protocol_write_only_traffic_stays_bounded():
    """走 protocol 入口的端到端断言:只写不读时容量也必须收敛。

    _handle_send 里 task_id 为空会短路掉 contains 读路径,所以这条流量真的一次读
    都没有 —— 正是原实现下表会一直长的场景。
    """
    from echo_agent.a2a.protocol import A2AProtocol

    clock = _Clock()

    async def stuck(task: A2ATask) -> A2ATask:
        return task  # 从不落终态

    proto = A2AProtocol(
        stuck, task_ttl_seconds=10.0, max_tasks=2,
        active_task_ttl_seconds=50.0, clock=clock,
    )
    for i in range(8):
        clock.now = i * 100.0  # 每轮都让上一轮越过 active 兜底
        await proto._handle_send({"id": "", "message": {"role": "user", "parts": []}})

    assert _raw_size(proto._tasks) <= 2, (
        f"纯写入流量下底层表未收敛,实际 {_raw_size(proto._tasks)} 条"
    )
