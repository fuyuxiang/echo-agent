"""Subprocess lifecycle: process-group isolation and full-tree reaping.

Regression coverage for the orphan-process / RSS-leak bug: on timeout or
error the shell must reap not just the direct child but its whole process
group (pipeline / backgrounded grandchildren), and never leave a zombie.
"""

from __future__ import annotations

import asyncio
import os
import signal

import pytest

from echo_agent.agent.proc_lifecycle import (
    _POSIX,
    process_group_alive,
    record_process_group,
    spawn_shell,
    subprocess_kwargs,
    terminate_tree,
)

# A grandchild that redirects its own stdio does NOT inherit the leader's pipes,
# so the leader's exit is observable immediately. Without the redirect,
# proc.wait() blocks on pipe EOF until the grandchild itself finishes, which
# hides the very race these tests target.
_BACKGROUND_CMD = "sleep 90 >/dev/null 2>&1 & echo $!"


async def _spawn_backgrounder() -> tuple[object, int]:
    """Spawn a shell that backgrounds a long sleep, then exits. Returns
    (proc, grandchild_pid) once the leader has exited and been reaped."""
    proc = await spawn_shell(
        _BACKGROUND_CMD,
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.PIPE,
    )
    line = await asyncio.wait_for(proc.stdout.readline(), timeout=5)
    grandchild = int(line.strip())
    await asyncio.wait_for(proc.wait(), timeout=5)
    assert proc.returncode == 0, "leader should exit immediately after backgrounding"
    return proc, grandchild


def test_subprocess_kwargs_starts_new_session_on_posix():
    kwargs = subprocess_kwargs()
    if _POSIX:
        assert kwargs == {"start_new_session": True}
    else:
        assert kwargs == {}


@pytest.mark.asyncio
async def test_terminate_tree_reaps_direct_child():
    proc = await asyncio.create_subprocess_shell("sleep 30", **subprocess_kwargs())
    assert proc.returncode is None
    await terminate_tree(proc)
    # Reaped: returncode is set, no zombie left behind.
    assert proc.returncode is not None


@pytest.mark.asyncio
async def test_terminate_tree_is_noop_on_exited_child():
    proc = await asyncio.create_subprocess_shell("true", **subprocess_kwargs())
    await proc.wait()
    rc = proc.returncode
    # Second reap over an already-exited child must not raise.
    await terminate_tree(proc)
    assert proc.returncode == rc


@pytest.mark.asyncio
@pytest.mark.skipif(not _POSIX, reason="process groups are POSIX-only")
async def test_terminate_tree_kills_grandchildren():
    """A backgrounded grandchild in the same group must die with the group.

    The shell backgrounds a long sleep and records its PID, then waits itself.
    Killing only the direct shell would leave the sleep orphaned; group reaping
    takes it out too.
    """
    # `sh -c 'sleep 60 & echo $! ; wait'` — the & child shares the new session's
    # process group. Read the grandchild PID from stdout before terminating.
    proc = await asyncio.create_subprocess_shell(
        "sleep 60 & echo $!; wait",
        stdout=asyncio.subprocess.PIPE,
        **subprocess_kwargs(),
    )
    line = await asyncio.wait_for(proc.stdout.readline(), timeout=5)
    grandchild_pid = int(line.strip())
    # Grandchild is alive (signal 0 probes existence).
    os.kill(grandchild_pid, 0)

    await terminate_tree(proc)

    # Poll until the grandchild is gone — it was signalled via the group.
    for _ in range(50):
        try:
            os.kill(grandchild_pid, 0)
        except ProcessLookupError:
            break
        await asyncio.sleep(0.1)
    else:
        pytest.fail("grandchild survived group termination — orphan leak")


@pytest.mark.asyncio
@pytest.mark.skipif(not _POSIX, reason="process groups are POSIX-only")
async def test_terminate_tree_escalates_to_sigkill():
    """A child that ignores SIGTERM is escalated to SIGKILL within grace."""
    # The shell traps SIGTERM and spins in-process (no separate sleep child that
    # a group SIGTERM could kill on its behalf). Only SIGKILL stops it, so the
    # leader's returncode proves escalation fired.
    proc = await asyncio.create_subprocess_shell(
        "trap '' TERM; while true; do :; done",
        **subprocess_kwargs(),
    )
    # Let the shell install its SIGTERM trap before we signal — otherwise the
    # early SIGTERM hits the default handler and the child dies at TERM, never
    # exercising the escalation path.
    await asyncio.sleep(0.5)
    await asyncio.wait_for(terminate_tree(proc, grace=1.0), timeout=10)
    assert proc.returncode is not None
    # Killed by SIGKILL (negative returncode == -SIGKILL).
    assert proc.returncode == -signal.SIGKILL


# ── leader 先退出的场景:进程组不能因此逃逸回收 ──────────────────────────────


@pytest.mark.asyncio
@pytest.mark.skipif(not _POSIX, reason="process groups are POSIX-only")
async def test_pgid_recorded_at_spawn_survives_leader_exit():
    """PGID 必须在 spawn 时记录 —— leader 被回收后就再也查不到它的组。

    os.getpgid() 需要一个活着且未被 reap 的 pid,而"leader 已退出"恰好是背景孙
    进程仍在运行、最需要回收整组的时刻。所以按需查询在唯一要紧的时点必然失败。
    """
    proc, grandchild = await _spawn_backgrounder()
    try:
        # 现场反查此时已失败,证明记录值是唯一可用信息。
        with pytest.raises(ProcessLookupError):
            os.getpgid(proc.pid)
        # 记录值仍在,且等于 leader 的 pid(start_new_session 的构造性保证)。
        assert getattr(proc, "_echo_pgid", None) == proc.pid
    finally:
        await terminate_tree(proc)
        _force_kill(grandchild)


@pytest.mark.asyncio
@pytest.mark.skipif(not _POSIX, reason="process groups are POSIX-only")
async def test_process_group_alive_distinguishes_tree_from_leader():
    """leader 退出 != 工作结束:组里还有背景孙进程时必须报告 alive。

    returncode 回答不了这个问题 —— 背景命令的 shell 立刻以 0 退出,而真正的工作
    还在同一个组里跑。ProcessTool 的表项回收依赖这个区分。
    """
    proc, grandchild = await _spawn_backgrounder()
    try:
        assert proc.returncode == 0, "leader 已退出"
        assert process_group_alive(proc) is True, "组内仍有孙进程,必须报告 alive"
    finally:
        await terminate_tree(proc)
        _force_kill(grandchild)
    assert process_group_alive(proc) is False, "回收后组必须为空"


@pytest.mark.asyncio
@pytest.mark.skipif(not _POSIX, reason="process groups are POSIX-only")
async def test_terminate_tree_reaps_group_after_leader_already_exited():
    """回归:leader 已有 returncode 时不能直接 return,否则孙进程永久逃逸。

    原实现开头是 `if proc.returncode is not None: return`,于是 shell 启动背景
    任务后退出,terminate_tree() 什么都不做就返回,背景进程一直活到 agent 进程
    自己死掉为止。
    """
    proc, grandchild = await _spawn_backgrounder()
    os.kill(grandchild, 0)  # 存活(signal 0 只探测存在性)

    await asyncio.wait_for(terminate_tree(proc, grace=2.0), timeout=15)

    # terminate_tree 返回即代表组已排空 —— 这是它的契约。
    assert process_group_alive(proc) is False, "孙进程逃过了整组回收"
    _force_kill(grandchild)


def _force_kill(pid: int) -> None:
    """兜底清理:测试失败时别把 sleep 留在机器上。"""
    try:
        os.kill(pid, signal.SIGKILL)
    except (ProcessLookupError, PermissionError):
        pass


@pytest.mark.asyncio
@pytest.mark.skipif(not _POSIX, reason="process groups are POSIX-only")
async def test_record_process_group_refuses_when_not_group_leader():
    """子进程没有自己的组时必须拒绝记录 —— 否则 killpg 会打到 agent 自己。

    不带 subprocess_kwargs() 启动的子进程共享父进程的组,把那个 pgid 记下来会让
    terminate_tree 向整个 agent 发信号。
    """
    proc = await asyncio.create_subprocess_shell("sleep 5")  # 故意不新建会话
    try:
        record_process_group(proc, own_session=False)
        assert getattr(proc, "_echo_pgid", None) is None, "共享父组时不得记录 pgid"
        assert process_group_alive(proc) is False, "无可信组 → 不得报告 alive"
    finally:
        await terminate_tree(proc)
