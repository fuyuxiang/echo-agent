"""Shared subprocess lifecycle helpers — process-group isolation and reaping.

Every subprocess we spawn goes through a shell (`/bin/sh -c "<cmd>"`), and a
command may itself fork: pipelines (`a | b`), backgrounded jobs (`x &`), or a
script that spawns its own children. Signalling only the direct child (the sh)
leaves those grandchildren reparented to init as orphans that keep running and
leak RSS.

The fix is two-part and lives here so every spawn site shares one
implementation:

- `subprocess_kwargs()` starts each child in a *new session*, making it the
  leader of its own process group. The whole tree then shares one PGID.
- `terminate_tree()` signals that PGID as a unit (SIGTERM, grace period, then
  SIGKILL) and always reaps the direct child with `wait()`, so nothing lingers
  as a zombie or an orphan.
"""

from __future__ import annotations

import asyncio
import os
import signal
from typing import Any

from loguru import logger

_POSIX = os.name == "posix"


def subprocess_kwargs() -> dict[str, Any]:
    """Spawn kwargs that put the child in its own process group.

    On POSIX, `start_new_session=True` runs `setsid()` in the child so its
    PGID equals its PID and every descendant inherits that group. On other
    platforms we spawn normally and fall back to single-process termination.
    """
    if _POSIX:
        return {"start_new_session": True}
    return {}


async def terminate_tree(proc: Any, *, grace: float = 5.0) -> None:
    """Terminate `proc` and its whole process group, then reap it.

    Escalation mirrors a graceful shutdown: SIGTERM the group, wait up to
    `grace` seconds, then SIGKILL the group. The direct child is always
    awaited so it cannot survive as a zombie. A no-op if the child already
    exited. Safe to call more than once.
    """
    if proc.returncode is not None:
        return
    _signal_group(proc, signal.SIGTERM)
    try:
        await asyncio.wait_for(proc.wait(), timeout=grace)
        return
    except asyncio.TimeoutError:
        pass
    _signal_group(proc, signal.SIGKILL)
    try:
        await proc.wait()
    except Exception as e:  # pragma: no cover - defensive
        logger.debug("wait() after SIGKILL failed for pid {}: {}", proc.pid, e)


def _signal_group(proc: Any, sig: int) -> None:
    """Signal the child's whole process group, falling back to the child alone.

    killpg is only used when the child is verified to be its own group leader
    (pgid == pid), which holds when `start_new_session` took effect. This guard
    is critical: if the child were NOT a leader, its PGID would be the parent's
    group and killpg would signal us. When we can't safely target the group we
    signal just the direct child.
    """
    pid = proc.pid
    if pid is None:
        return
    if _POSIX:
        try:
            pgid = os.getpgid(pid)
            if pgid == pid:  # child is its own group leader — safe to killpg
                os.killpg(pgid, sig)
                return
        except ProcessLookupError:
            return  # already gone and reaped
        except Exception as e:  # pragma: no cover - defensive
            logger.debug("killpg({}, {}) failed, falling back: {}", pid, sig, e)
    # Fallback: signal only the direct child.
    try:
        if sig == signal.SIGKILL:
            proc.kill()
        else:
            proc.terminate()
    except ProcessLookupError:
        pass
    except Exception as e:  # pragma: no cover - defensive
        logger.debug("single-process signal for pid {} failed: {}", pid, e)
