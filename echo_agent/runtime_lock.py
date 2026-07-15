"""Workspace-scoped single-instance lock.

Guarantees that at most one channel-consuming agent process runs per workspace.
Without it, a background service (``echo-agent gateway``) and a foreground
``echo-agent run`` against the same workspace both start their channel pollers,
so every inbound message is consumed — and answered — twice. The workspace is
the right scope because it is the shared root for the SQLite database, the
scheduler state and the memory store; one lock there protects all of them.

The authority is the OS advisory lock, never the file contents: an ``flock``
(POSIX) / ``msvcrt`` (Windows) lock is released automatically when the holding
process dies, so a crash never leaves a stale lock behind — unlike a bare
pidfile, which would need liveness + start-time reconciliation to clean up. The
pid / start_time / role written into the file are for human diagnostics only.
"""

from __future__ import annotations

import os
from dataclasses import dataclass
from pathlib import Path

from loguru import logger

try:
    import fcntl  # POSIX
    _HAS_FCNTL = True
except ImportError:  # pragma: no cover - platform-specific
    fcntl = None  # type: ignore[assignment]
    _HAS_FCNTL = False

try:
    import msvcrt  # Windows
    _HAS_MSVCRT = True
except ImportError:  # pragma: no cover - platform-specific
    msvcrt = None  # type: ignore[assignment]
    _HAS_MSVCRT = False

LOCK_FILENAME = "agent.lock"


class InstanceLockError(Exception):
    """Raised when the workspace lock is already held by another live process.

    ``message`` is a user-facing, actionable hint; ``holder_pid`` is the PID
    recorded in the lock file (best-effort, may be None on a race)."""

    def __init__(self, message: str, holder_pid: int | None = None):
        super().__init__(message)
        self.message = message
        self.holder_pid = holder_pid


@dataclass
class InstanceLock:
    """Handle for an acquired workspace lock. Call :meth:`release` to drop it.

    Held for the lifetime of the agent process; :class:`AppRuntime` acquires it
    at the start of its lifecycle and releases it on stop."""

    path: Path
    _fd: object = None  # file object holding the OS lock (None when no-op)

    def release(self) -> None:
        fd = self._fd
        self._fd = None
        if fd is None:
            return
        try:
            if _HAS_FCNTL:
                fcntl.flock(fd, fcntl.LOCK_UN)  # type: ignore[union-attr]
            elif _HAS_MSVCRT:
                try:
                    fd.seek(0)  # type: ignore[union-attr]
                    msvcrt.locking(fd.fileno(), msvcrt.LK_UNLCK, 1)  # type: ignore[union-attr]
                except OSError:
                    pass
        finally:
            try:
                fd.close()  # type: ignore[union-attr]
            except OSError:
                pass


def _lock_path(workspace: Path) -> Path:
    return workspace / "data" / LOCK_FILENAME


def _read_holder_pid(path: Path) -> int | None:
    """Best-effort read of the PID recorded by the current holder (diagnostics)."""
    try:
        first = path.read_text(encoding="utf-8").splitlines()[0].strip()
        return int(first.split("=", 1)[1]) if first.startswith("pid=") else None
    except (OSError, ValueError, IndexError):
        return None


def _conflict_message(workspace: Path, holder_pid: int | None) -> str:
    pid_hint = f"（PID {holder_pid}）" if holder_pid else ""
    return (
        f"该 workspace 已有一个 echo-agent 在运行{pid_hint}：{workspace}\n"
        "两个实例会重复消费通道消息、重复回复。请二选一：\n"
        "  • 接入正在运行的实例： echo-agent cli\n"
        "  • 另起独立实例：换一个 --workspace 目录\n"
        "  • 确认要强制多开（会造成重复回复/并发写库风险）： 加 --force"
    )


def acquire_instance_lock(workspace: Path, role: str = "run") -> InstanceLock:
    """Acquire the workspace single-instance lock (non-blocking).

    Returns an :class:`InstanceLock` on success. Raises :class:`InstanceLockError`
    if another live process already holds it. On platforms without any advisory
    lock primitive the lock degrades to a no-op handle (best-effort, mirrors the
    scheduler's behaviour) so the agent still runs.
    """
    path = _lock_path(workspace)
    path.parent.mkdir(parents=True, exist_ok=True)

    if not _HAS_FCNTL and not _HAS_MSVCRT:  # pragma: no cover - platform-specific
        logger.warning(
            "当前平台无文件锁原语，跳过单实例互斥；请自行确保同一 workspace 只启动一个实例"
        )
        return InstanceLock(path=path, _fd=None)

    # msvcrt.locking needs a non-empty region to lock; seed one byte.
    if _HAS_MSVCRT and (not path.exists() or path.stat().st_size == 0):
        path.write_text(" ", encoding="utf-8")

    fd = open(path, "r+" if _HAS_MSVCRT else "a+", encoding="utf-8")
    try:
        if _HAS_FCNTL:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)  # type: ignore[union-attr]
        else:
            fd.seek(0)
            msvcrt.locking(fd.fileno(), msvcrt.LK_NBLCK, 1)  # type: ignore[union-attr]
    except OSError as e:
        fd.close()
        holder = _read_holder_pid(path)
        raise InstanceLockError(_conflict_message(workspace, holder), holder_pid=holder) from e

    _write_holder_info(fd, role)
    logger.debug("Acquired workspace instance lock: {} (role={})", path, role)
    return InstanceLock(path=path, _fd=fd)


def _write_holder_info(fd, role: str) -> None:
    """Record diagnostics into the held lock file. Never authoritative."""
    import time

    try:
        fd.seek(0)
        fd.truncate()
        fd.write(f"pid={os.getpid()}\nrole={role}\nstart_time={int(time.time())}\n")
        fd.flush()
    except OSError:
        pass  # diagnostics only; the OS lock is what matters
