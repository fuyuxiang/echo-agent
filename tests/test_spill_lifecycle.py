"""清扫循环的生命周期:不占共享后台槽,关掉 spill 后仍继续回收。

sweep_forever 永不返回。把它放进有界的 BackgroundScheduler 有两种失效:启动
时池已满则被永久丢弃(不是漏一轮,是再也不清扫);启动成功则永久占一个信号量
槽,并发上限小时把后续 DURABLE 全部堵死。
"""

from __future__ import annotations

import asyncio
import os
import time
from pathlib import Path

import pytest

from echo_agent.agent.background import BackgroundScheduler, Tier
from echo_agent.spill.layout import session_dir_name
from echo_agent.spill.sweeper import sweep_forever


def _artifact(root: Path, name: str, age_days: float) -> Path:
    p = root / session_dir_name("sess-a") / name
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text("z" * 1024, encoding="utf-8")
    old = time.time() - age_days * 86400
    os.utime(p, (old, old))
    return p


@pytest.mark.asyncio
async def test_sweep_forever_does_not_occupy_a_scheduler_slot():
    """并发上限为 1 时,清扫循环不得让 DURABLE 工作饿死。

    这里直接示范"放进调度器会怎样":sweep_forever 占住唯一的槽,durable 永远
    跑不上。生产代码因此改用独立 asyncio.Task。
    """
    scheduler = BackgroundScheduler(max_concurrency=1)
    ran = asyncio.Event()

    async def durable_work():
        ran.set()

    scheduler.spawn(sweep_forever(Path("nonexistent"), 7, 512, 6))
    scheduler.spawn(durable_work, tier=Tier.DURABLE)
    with pytest.raises(asyncio.TimeoutError):
        await asyncio.wait_for(ran.wait(), timeout=0.3)
    await scheduler.aclose(timeout=1.0)


@pytest.mark.asyncio
async def test_sweep_forever_sweeps_immediately_then_can_be_cancelled(tmp_path):
    """启动即扫一次,之后停在 sleep 上,cancel 立刻收得住。"""
    stale = _artifact(tmp_path, "aaaaaaaa-exec.txt", age_days=30)
    task = asyncio.create_task(sweep_forever(tmp_path, 7, 512, 6))
    for _ in range(200):
        await asyncio.sleep(0.01)
        if not stale.exists():
            break
    assert not stale.exists()

    task.cancel()
    with pytest.raises(asyncio.CancelledError):
        await task


@pytest.mark.asyncio
async def test_sweep_runs_off_the_event_loop(tmp_path, monkeypatch):
    """扫描必须下放到工作线程,否则产物多时会冻住通道轮询与健康检查。"""
    _artifact(tmp_path, "aaaaaaaa-exec.txt", age_days=30)
    loop_thread = asyncio.get_running_loop() and __import__("threading").get_ident()
    seen: list[int] = []

    import echo_agent.spill.sweeper as sweeper_mod
    real_sweep = sweeper_mod.sweep

    def recording_sweep(*args, **kwargs):
        seen.append(__import__("threading").get_ident())
        return real_sweep(*args, **kwargs)

    monkeypatch.setattr(sweeper_mod, "sweep", recording_sweep)
    task = asyncio.create_task(sweep_forever(tmp_path, 7, 512, 6))
    for _ in range(200):
        await asyncio.sleep(0.01)
        if seen:
            break
    task.cancel()
    with pytest.raises(asyncio.CancelledError):
        await task
    assert seen and seen[0] != loop_thread


@pytest.mark.asyncio
async def test_sweep_forever_survives_a_failing_sweep(tmp_path, monkeypatch):
    """一轮扫描抛异常不能终结循环——否则一次瞬时 IO 错误就永久停掉清扫。"""
    calls: list[int] = []

    import echo_agent.spill.sweeper as sweeper_mod

    def flaky_sweep(*args, **kwargs):
        calls.append(1)
        raise OSError("transient")

    monkeypatch.setattr(sweeper_mod, "sweep", flaky_sweep)
    # interval 的下界是 1 小时,所以第二轮不会在测试里到来;这里只验证第一轮
    # 抛异常之后任务仍存活(停在 sleep 上),而不是被异常带走。
    task = asyncio.create_task(sweep_forever(tmp_path, 7, 512, 6))
    for _ in range(200):
        await asyncio.sleep(0.01)
        if calls:
            break
    await asyncio.sleep(0.05)
    assert calls
    assert not task.done()
    task.cancel()
    with pytest.raises(asyncio.CancelledError):
        await task


class _FakeLoop:
    """只保留 _start_spill_sweeper 需要的那几个属性,避免构造整个 AgentLoop。"""

    _start_spill_sweeper = None  # 由下方 fixture 绑定真实实现

    def __init__(self, root: Path, enabled: bool):
        from types import SimpleNamespace
        self._spill_store = SimpleNamespace(root=root)
        self.config = SimpleNamespace(spill=SimpleNamespace(
            enabled=enabled, retention_days=7, max_total_mb=512, sweep_interval_hours=6,
        ))
        self._spill_sweep_task = None
        self._spawned: list = []

    def _spawn_background(self, coro, *, tier=None):
        # 生产代码若回退到调度器,这里会被调用——测试据此失败。
        self._spawned.append(coro)
        coro.close()


@pytest.mark.parametrize("enabled", [True, False])
@pytest.mark.asyncio
async def test_sweeper_starts_regardless_of_spill_enabled(tmp_path, enabled):
    """关掉 spill 只是不再产生新产物,已有敏感内容仍须继续受保留期约束。

    运维把开关关掉后,历史产物如果就此永不回收,retentionDays/maxTotalMb 对
    磁盘上真正敏感的那批内容全部失效——恰好是最需要它们生效的时候。
    """
    from echo_agent.agent.loop import AgentLoop

    stale = _artifact(tmp_path, "aaaaaaaa-exec.txt", age_days=30)
    fake = _FakeLoop(tmp_path, enabled=enabled)
    AgentLoop._start_spill_sweeper(fake)

    task = fake._spill_sweep_task
    assert task is not None
    # 不经调度器:清扫循环永不返回,占槽会饿死 DURABLE 工作。
    assert fake._spawned == []
    for _ in range(200):
        await asyncio.sleep(0.01)
        if not stale.exists():
            break
    assert not stale.exists()

    task.cancel()
    with pytest.raises(asyncio.CancelledError):
        await task
