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
    subprocess_kwargs,
    terminate_tree,
)


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
