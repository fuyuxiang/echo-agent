"""tasks/cancel 必须真正终止任务,且终态不可被后到的结果覆盖。

原实现的 cancel 只改可变任务对象的 state 字段:没有任何句柄能停下正在 await 的
_process,而 worker 返回后 _handle_send 又无条件回写 store,把 CANCELED 覆盖成
COMPLETED。对调用方而言就是 cancel 应答 canceled、随后 tasks/get 却拿到
completed,期间工具副作用一直在继续。
"""

from __future__ import annotations

import asyncio

import pytest

from echo_agent.a2a.models import A2AMessage, A2ATask, TaskState
from echo_agent.a2a.protocol import A2AProtocol


def _msg() -> dict:
    return {"role": "user", "parts": [{"type": "text", "text": "hi"}]}


@pytest.mark.asyncio
async def test_cancel_stops_the_running_worker():
    """cancel 必须取消在跑的 _process,而不是任由它跑完。

    用 side_effects 记录 worker 在取消点之后还做了什么 —— 原实现里这些副作用会
    全部发生,因为 cancel 从来没碰到过运行中的协程。
    """
    entered = asyncio.Event()
    release = asyncio.Event()
    side_effects: list[str] = []

    async def slow(task: A2ATask) -> A2ATask:
        entered.set()
        try:
            await release.wait()
        except asyncio.CancelledError:
            side_effects.append("cancelled")
            raise
        # 原实现会走到这里:取消后工具仍在执行、状态仍被改成 COMPLETED。
        side_effects.append("kept-working")
        task.state = TaskState.COMPLETED
        task.messages.append(A2AMessage.text("agent", "done"))
        return task

    proto = A2AProtocol(slow)
    send = asyncio.create_task(proto._handle_send({"id": "k1", "message": _msg()}))
    await asyncio.wait_for(entered.wait(), timeout=5)

    result = proto._handle_cancel({"id": "k1"})
    assert result["state"] == "canceled"

    # Bounded: without a real cancel the worker parks on `release` forever.
    try:
        await asyncio.wait_for(asyncio.shield(send), timeout=5)
    except asyncio.CancelledError:
        pass
    except asyncio.TimeoutError:
        send.cancel()
        pytest.fail("cancel 没有停下 worker,_handle_send 仍在等待")

    assert side_effects == ["cancelled"], f"worker 在取消后仍在工作: {side_effects}"


@pytest.mark.asyncio
async def test_canceled_task_is_not_revived_by_worker_result():
    """回归:worker 返回后不得把已确认的 CANCELED 覆盖成 COMPLETED。

    这里的 worker 故意吞掉 CancelledError 并照常返回 COMPLETED —— 模拟一个不配合
    取消的实现。即使如此,已经应答给调用方的终态也必须留住。
    """
    entered = asyncio.Event()

    async def swallows_cancel(task: A2ATask) -> A2ATask:
        entered.set()
        try:
            await asyncio.sleep(3600)
        except asyncio.CancelledError:
            pass  # 不配合:把取消吞掉
        task.state = TaskState.COMPLETED
        task.messages.append(A2AMessage.text("agent", "done anyway"))
        return task

    proto = A2AProtocol(swallows_cancel)
    send = asyncio.create_task(proto._handle_send({"id": "k2", "message": _msg()}))
    await asyncio.wait_for(entered.wait(), timeout=5)

    assert proto._handle_cancel({"id": "k2"})["state"] == "canceled"
    # Bounded wait: an implementation that never cancels the worker would park on
    # the worker's own sleep forever, and a hanging test reports nothing useful.
    try:
        await asyncio.wait_for(asyncio.shield(send), timeout=5)
    except asyncio.CancelledError:
        pass
    except asyncio.TimeoutError:
        send.cancel()
        pytest.fail("cancel 没有停下 worker,_handle_send 仍在等待")

    # 调用方视角的唯一事实源:tasks/get 必须仍然是 canceled。
    assert proto._handle_get({"id": "k2"})["state"] == "canceled", "已确认的终态被复活了"


@pytest.mark.asyncio
async def test_canceled_task_cannot_revive_after_result_ttl_expires():
    """The cancel promise outlives its row while a stubborn worker is alive."""

    class _Clock:
        now = 0.0

        def __call__(self) -> float:
            return self.now

    clock = _Clock()
    entered = asyncio.Event()
    first_cancel = asyncio.Event()
    second_cancel = asyncio.Event()
    release = asyncio.Event()

    async def swallows_repeated_cancel(task: A2ATask) -> A2ATask:
        entered.set()
        try:
            await release.wait()
        except asyncio.CancelledError:
            first_cancel.set()
            try:
                await release.wait()
            except asyncio.CancelledError:
                second_cancel.set()
                await release.wait()
        task.state = TaskState.COMPLETED
        return task

    proto = A2AProtocol(
        swallows_repeated_cancel, task_ttl_seconds=10.0, clock=clock,
    )
    send = asyncio.create_task(proto._handle_send({"id": "ttl-cancel", "message": _msg()}))
    await asyncio.wait_for(entered.wait(), timeout=1)

    assert proto._handle_cancel({"id": "ttl-cancel"})["state"] == "canceled"
    await asyncio.wait_for(first_cancel.wait(), timeout=1)

    clock.now = 11.0
    assert proto._tasks.get("ttl-cancel") is None
    await asyncio.wait_for(second_cancel.wait(), timeout=1)
    assert "ttl-cancel" in proto._settled

    release.set()
    assert (await asyncio.wait_for(send, timeout=1))["state"] == "canceled"
    # The expired row stays expired and the temporary fence is released only
    # after the late result has been neutralised.
    assert proto._tasks.get("ttl-cancel") is None
    assert "ttl-cancel" not in proto._settled
    assert "ttl-cancel" not in proto._reclaimed_settlements


@pytest.mark.asyncio
async def test_cancel_fence_survives_ttl_purge_after_worker_done_before_commit():
    """A done worker is not yet an observed/committed worker result."""
    now = 0.0
    proto = None
    entered = asyncio.Event()

    async def stubborn_worker(task: A2ATask) -> A2ATask:
        nonlocal now
        entered.set()
        try:
            await asyncio.Event().wait()
        except asyncio.CancelledError:
            now = 11.0
            task.state = TaskState.COMPLETED
            # This callback runs after the worker completes but can run before
            # _handle_send resumes from ``await run``.
            asyncio.get_running_loop().call_soon(proto._tasks.get, task.id)
            return task

    proto = A2AProtocol(
        stubborn_worker,
        task_ttl_seconds=10.0,
        clock=lambda: now,
    )
    send = asyncio.create_task(proto._handle_send({
        "id": "done-race",
        "message": _msg(),
    }))
    await entered.wait()

    assert proto._handle_cancel({"id": "done-race"})["state"] == "canceled"
    assert (await send)["state"] == "canceled"
    assert proto._tasks.get("done-race") is None
    assert proto._settled == {}
    assert proto._reclaimed_settlements == set()


@pytest.mark.asyncio
async def test_get_never_observes_late_worker_mutation_after_cancel():
    entered = asyncio.Event()
    mutated = asyncio.Event()
    release = asyncio.Event()

    async def mutates_then_waits(task: A2ATask) -> A2ATask:
        entered.set()
        try:
            await asyncio.Event().wait()
        except asyncio.CancelledError:
            task.state = TaskState.COMPLETED
            mutated.set()
            await release.wait()
            return task

    proto = A2AProtocol(mutates_then_waits)
    send = asyncio.create_task(proto._handle_send({
        "id": "observable-race",
        "message": _msg(),
    }))
    await entered.wait()

    assert proto._handle_cancel({"id": "observable-race"})["state"] == "canceled"
    await mutated.wait()
    assert proto._handle_get({"id": "observable-race"})["state"] == "canceled"

    release.set()
    assert (await send)["state"] == "canceled"


@pytest.mark.asyncio
async def test_cancel_after_completion_still_rejected():
    """任务已自然完成时 cancel 仍必须报错 —— 终态不可再迁移。"""

    async def quick(task: A2ATask) -> A2ATask:
        task.state = TaskState.COMPLETED
        return task

    proto = A2AProtocol(quick)
    await proto._handle_send({"id": "k3", "message": _msg()})
    with pytest.raises(ValueError, match="cannot be canceled"):
        proto._handle_cancel({"id": "k3"})


@pytest.mark.asyncio
async def test_run_handles_do_not_accumulate():
    """运行句柄表只能在飞:每轮结束都要清掉,否则又是一处无上界增长。"""

    async def quick(task: A2ATask) -> A2ATask:
        task.state = TaskState.COMPLETED
        return task

    proto = A2AProtocol(quick)
    for i in range(5):
        await proto._handle_send({"id": f"r{i}", "message": _msg()})
    assert proto._runs == {}, f"结束的运行句柄未清理: {list(proto._runs)}"


@pytest.mark.asyncio
async def test_revived_task_can_be_canceled_again():
    """复活(对已完成任务再 send)后必须重新可取消。

    终态确认是"本轮"的承诺;新一轮开始就不该再受上一轮的终态约束,否则复活的任务
    永远无法取消。
    """
    gate = asyncio.Event()
    entered = asyncio.Event()

    async def first(task: A2ATask) -> A2ATask:
        task.state = TaskState.COMPLETED
        return task

    proto = A2AProtocol(first)
    await proto._handle_send({"id": "r9", "message": _msg()})
    assert proto._handle_get({"id": "r9"})["state"] == "completed"

    async def second(task: A2ATask) -> A2ATask:
        entered.set()
        await gate.wait()
        return task

    proto._process = second
    send = asyncio.create_task(proto._handle_send({"id": "r9", "message": _msg()}))
    await asyncio.wait_for(entered.wait(), timeout=5)

    assert proto._handle_cancel({"id": "r9"})["state"] == "canceled"
    try:
        await asyncio.wait_for(asyncio.shield(send), timeout=5)
    except asyncio.CancelledError:
        pass
    except asyncio.TimeoutError:
        send.cancel()
        pytest.fail("复活后的任务无法被取消")


@pytest.mark.asyncio
async def test_settled_bookkeeping_released_with_the_task():
    """任务被 store 回收时,protocol 的旁表必须同步清理。

    否则"给 store 加上界"这件事只是把泄漏搬到了旁表里。
    """

    class _Clock:
        def __init__(self) -> None:
            self.now = 0.0

        def __call__(self) -> float:
            return self.now

    clock = _Clock()

    async def quick(task: A2ATask) -> A2ATask:
        task.state = TaskState.COMPLETED
        return task

    proto = A2AProtocol(quick, task_ttl_seconds=10.0, max_tasks=100, clock=clock)
    proto._process = quick
    task = A2ATask(id="s1", state=TaskState.WORKING)
    proto._tasks["s1"] = task
    proto._handle_cancel({"id": "s1"})
    assert "s1" in proto._settled

    clock.now = 100.0
    assert proto._tasks.get("s1") is None  # 触发 purge
    assert "s1" not in proto._settled, "旁表未随任务回收"
