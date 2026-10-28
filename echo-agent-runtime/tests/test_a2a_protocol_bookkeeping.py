"""A2AProtocol 与任务仓库的记账闭环。

已有的 store 测试都直接 `store[k] = task`,绕过了 protocol 的真实调用序列 ——
两个 P0 泄漏(cancel 不回写、异常路径残留 WORKING)正是因此逃过检测。这里的用例
一律走 protocol 入口。
"""

from __future__ import annotations

import asyncio

import pytest

from echo_agent.a2a.models import TaskState
from echo_agent.a2a.protocol import A2AProtocol


class _Clock:
    def __init__(self) -> None:
        self.now = 0.0

    def __call__(self) -> float:
        return self.now


def _msg() -> dict:
    return {"role": "user", "parts": [{"type": "text", "text": "hi"}]}


async def _completes(task):
    task.state = TaskState.COMPLETED
    return task


def _working(task_id: str):
    from echo_agent.a2a.models import A2ATask
    return A2ATask(id=task_id, state=TaskState.WORKING)


@pytest.mark.asyncio
async def test_cancel_arms_ttl_and_frees_capacity():
    # cancel 原地改 state 却不回写 store 时,_expire_at 永不 arm,且容量淘汰
    # 会把 CANCELED 误判成活跃任务而整体停摆。
    # 断言 _expire_at 而非仅看回收结果:活跃任务兜底(active_ttl)也能回收条目,
    # 会掩盖"TTL 从未 arm"这个真实缺陷,故此处把 active_ttl 设到极大值排除它。
    clock = _Clock()
    p = A2AProtocol(
        _completes, task_ttl_seconds=10.0, max_tasks=2,
        active_task_ttl_seconds=1e9, clock=clock,
    )
    for i in range(5):
        task_id = f"c{i}"
        p._tasks[task_id] = _working(task_id)
        p._handle_cancel({"id": task_id})
        assert task_id in p._tasks._expire_at, "cancel 后必须 arm TTL(说明已回写 store)"

    assert len(p._tasks) <= 2, "CANCELED 是终态,必须参与容量淘汰"
    clock.now = 1e6  # 远超终态 TTL,但远未到 active 兜底
    assert len(p._tasks) == 0, "取消的任务必须在 TTL 后被回收"


@pytest.mark.asyncio
async def test_cancelled_send_does_not_strand_working_task():
    # 客户端断连 → aiohttp cancel 请求协程 → CancelledError 从 _process 抛出。
    # 若不落终态,条目卡在 WORKING:免疫 TTL 又免疫容量淘汰。
    clock = _Clock()

    async def disconnects(task):
        raise asyncio.CancelledError()

    p = A2AProtocol(
        disconnects, task_ttl_seconds=10.0, max_tasks=2,
        active_task_ttl_seconds=1e9, clock=clock,
    )
    for i in range(6):
        with pytest.raises(asyncio.CancelledError):
            await p._handle_send({"id": f"w{i}", "message": _msg()})
        # 同上:把 active 兜底排除在外,直接验证任务真的落了终态并 arm 了 TTL。
        assert f"w{i}" in p._tasks._expire_at, "断连任务必须落终态并 arm TTL"

    assert len(p._tasks) <= 2, "断连任务必须落终态并受容量约束"
    clock.now = 1e6
    assert len(p._tasks) == 0, "断连任务必须在 TTL 后被回收"


@pytest.mark.asyncio
async def test_failed_send_is_reclaimable():
    clock = _Clock()

    async def boom(task):
        raise RuntimeError("upstream exploded")

    p = A2AProtocol(boom, task_ttl_seconds=10.0, max_tasks=100, clock=clock)
    with pytest.raises(RuntimeError):
        await p._handle_send({"id": "f1", "message": _msg()})
    assert p._tasks.get("f1").state == TaskState.FAILED
    clock.now = 11.0
    assert "f1" not in p._tasks


@pytest.mark.asyncio
async def test_revived_task_not_purged_while_in_flight():
    # 复活路径:已完成任务再 send 时若不重置记账,上一轮 arm 的旧 deadline 会
    # 让读路径在任务仍在处理中时把它清掉,调用方拿到 Task not found。
    clock = _Clock()
    entered = asyncio.Event()
    release = asyncio.Event()

    async def slow(task):
        entered.set()
        await release.wait()
        task.state = TaskState.COMPLETED
        return task

    p = A2AProtocol(_completes, task_ttl_seconds=10.0, max_tasks=100, clock=clock)
    await p._handle_send({"id": "r1", "message": _msg()})  # 首轮完成,arm 到 t=10

    p._process = slow
    clock.now = 5.0
    second = asyncio.create_task(p._handle_send({"id": "r1", "message": _msg()}))
    await entered.wait()

    clock.now = 11.0  # 越过首轮的旧 deadline
    assert p._handle_get({"id": "r1"})["id"] == "r1", "在飞任务不得被旧 deadline 清掉"

    release.set()
    await second
