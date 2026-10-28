"""MCP stdio process-tree reclamation.

The transport spawned the server with ``create_subprocess_exec`` and, on close,
signalled only that direct child. An MCP server is very often a launcher
(``npx``, ``uvx``, a shell wrapper) whose real work runs in a grandchild, so
``stop``, a retry, or a failed handshake could leave that grandchild running —
an orphan still holding whatever port or credential it was handed. Worse,
``close()`` returned early when ``returncode is not None``, which is precisely
the case where a launcher has exited and left its tree behind.

These tests spawn real processes; each asserts on the *tree*, not on the leader.
"""

from __future__ import annotations

import asyncio
import os
import signal
import sys

import pytest

from echo_agent.mcp.transport import StdioTransport

pytestmark = pytest.mark.skipif(
    os.name != "posix", reason="process groups are POSIX-only"
)


def _alive(pid: int) -> bool:
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:  # pragma: no cover - exists but not ours
        return True
    return True


async def _wait_gone(pid: int, timeout: float = 5.0) -> bool:
    loop = asyncio.get_running_loop()
    deadline = loop.time() + timeout
    while loop.time() < deadline:
        if not _alive(pid):
            return True
        await asyncio.sleep(0.05)
    return not _alive(pid)


#: A "server" that forks a long-lived grandchild, announces both pids on stdout,
#: then keeps running. Mirrors the launcher shape that used to leak.
_LAUNCHER = """
import os, subprocess, sys, time
child = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(300)"])
sys.stdout.write("%d %d\\n" % (os.getpid(), child.pid))
sys.stdout.flush()
time.sleep(300)
"""

#: A launcher that *exits* right after spawning the grandchild. This is the case
#: the old early-return on `returncode is not None` skipped entirely.
_EXITING_LAUNCHER = """
import os, subprocess, sys
child = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(300)"])
sys.stdout.write("%d %d\\n" % (os.getpid(), child.pid))
sys.stdout.flush()
"""


async def _start(script: str) -> tuple[StdioTransport, int, int]:
    transport = StdioTransport(command=sys.executable, args=["-u", "-c", script])
    await transport.connect(timeout=30)
    assert transport._process is not None and transport._process.stdout is not None
    line = await asyncio.wait_for(transport._process.stdout.readline(), timeout=30)
    leader, grandchild = (int(part) for part in line.split())
    return transport, leader, grandchild


@pytest.mark.asyncio
async def test_disconnect_reclaims_the_grandchild():
    transport, leader, grandchild = await _start(_LAUNCHER)
    try:
        assert _alive(grandchild), "fixture did not actually spawn a grandchild"
        await transport.close()
        assert await _wait_gone(grandchild), "grandchild outlived the transport"
        assert await _wait_gone(leader)
    finally:
        for pid in (grandchild, leader):
            if _alive(pid):
                try:
                    os.kill(pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass


async def _wait_reaped(proc, timeout: float = 30.0) -> None:
    """Wait until *proc*'s returncode is set, without calling ``wait()``.

    ``wait()`` is unusable here: asyncio resolves it only once the process has
    exited *and* every pipe transport has closed, and the grandchild inherited
    this launcher's stdout/stderr — so ``wait()`` would block for the
    grandchild's whole lifetime even though ``returncode`` is already set. That
    asymmetry is also why the production early-return on ``returncode is not
    None`` was reachable in the first place.
    """
    loop = asyncio.get_running_loop()
    deadline = loop.time() + timeout
    while proc.returncode is None and loop.time() < deadline:
        await asyncio.sleep(0.05)
    assert proc.returncode is not None, "launcher never exited"


@pytest.mark.asyncio
async def test_already_exited_launcher_still_gets_its_tree_swept():
    """`returncode is not None` was a shortcut, not a guarantee of cleanliness."""
    transport, leader, grandchild = await _start(_EXITING_LAUNCHER)
    try:
        assert transport._process is not None
        # Let the launcher finish, so close() sees a non-None returncode.
        await _wait_reaped(transport._process)
        assert _alive(grandchild), "fixture's grandchild died on its own"

        await transport.close()

        assert await _wait_gone(grandchild), (
            "an exited launcher's grandchild escaped the sweep"
        )
    finally:
        if _alive(grandchild):
            try:
                os.kill(grandchild, signal.SIGKILL)
            except ProcessLookupError:
                pass


@pytest.mark.asyncio
async def test_close_is_idempotent():
    transport, leader, grandchild = await _start(_LAUNCHER)
    try:
        await transport.close()
        await transport.close()  # must not raise on an empty group
        assert transport.is_connected is False
    finally:
        for pid in (grandchild, leader):
            if _alive(pid):
                try:
                    os.kill(pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass


@pytest.mark.asyncio
async def test_server_runs_in_its_own_process_group():
    """The precondition for group termination — and for it being safe.

    If setsid() had not taken effect the child would share the *agent's* group,
    and signalling that group would take down the test runner itself.
    """
    transport, leader, grandchild = await _start(_LAUNCHER)
    try:
        assert os.getpgid(leader) == leader
        assert os.getpgid(grandchild) == leader
        assert os.getpgid(leader) != os.getpgid(os.getpid())
    finally:
        await transport.close()
        for pid in (grandchild, leader):
            if _alive(pid):
                try:
                    os.kill(pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
