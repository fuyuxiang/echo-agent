"""Shared subprocess lifecycle helpers — process-group isolation and reaping.

Every subprocess we spawn goes through a shell (`/bin/sh -c "<cmd>"`), and a
command may itself fork: pipelines (`a | b`), backgrounded jobs (`x &`), or a
script that spawns its own children. Signalling only the direct child (the sh)
leaves those grandchildren reparented to init as orphans that keep running and
leak RSS.

The fix is three-part and lives here so every spawn site shares one
implementation:

- `spawn_shell()` / `spawn_exec()` start each child in a *new session*, making
  it the leader of its own process group, and record that group id while the
  leader is still alive. The whole tree shares one PGID.
- `terminate_tree()` signals that PGID as a unit (SIGTERM, grace period, then
  SIGKILL) and always reaps the direct child with `wait()`, so nothing lingers
  as a zombie or an orphan. Crucially it sweeps the group even when the leader
  has *already* exited — a shell that backgrounds a job (`sleep 300 &`) exits
  immediately while its grandchild keeps running, so "leader is gone" says
  nothing about whether the tree is gone.
- `communicate_owned()` / `run_owned()` are the async and blocking completion
  primitives for one-shot commands. They preserve the operation's
  result/exception but unconditionally sweep the recorded group, including
  after a successful/non-zero leader exit. Async cleanup is shielded to
  convergence so cancellation cannot abandon the only tree handle.
- `process_group_alive()` answers "is anything still running in this tree?" so
  callers that keep bookkeeping per child (ProcessTool) can tell a genuinely
  finished process from one whose leader merely exited first.

Why the PGID is recorded at spawn instead of looked up on demand: `os.getpgid()`
needs a live, unreaped pid. Once the leader is reaped its pgid is unknowable,
which is exactly the case where background grandchildren survive — so an
on-demand lookup fails at the only moment it matters. With
`start_new_session=True` the group id equals the child's pid by construction, so
recording it at spawn is both cheap and exact.
"""

from __future__ import annotations

import asyncio
import os
import signal
import subprocess
import time
from typing import Any

from loguru import logger

_POSIX = os.name == "posix"

# Attribute stamped on the Process object holding the *trusted* PGID. Trusted
# means: we spawned this child into its own session, so signalling the group
# cannot reach anything but its descendants. Absence means "fall back to the
# conservative live lookup" — never "assume pid == pgid".
_PGID_ATTR = "_echo_pgid"

# Poll interval while waiting for a signalled group to drain. Grandchildren are
# not our children, so `wait()` is not available for them; existence probing
# (`killpg(pgid, 0)`) is the only portable way to observe the group emptying.
_GROUP_POLL_INTERVAL = 0.05


def subprocess_kwargs() -> dict[str, Any]:
    """Spawn kwargs that put the child in its own process group.

    On POSIX, `start_new_session=True` runs `setsid()` in the child so its
    PGID equals its PID and every descendant inherits that group. On other
    platforms we spawn normally and fall back to single-process termination.
    """
    if _POSIX:
        return {"start_new_session": True}
    return {}


async def spawn_shell(cmd: str, **kwargs: Any) -> Any:
    """`create_subprocess_shell` in its own process group, PGID recorded.

    The single spawn entry point for shell commands. Callers must not apply
    `subprocess_kwargs()` themselves — doing both is harmless but signals that
    the caller is reasoning about grouping, which is this module's job.
    """
    proc = await asyncio.create_subprocess_shell(cmd, **{**subprocess_kwargs(), **kwargs})
    record_process_group(proc, own_session=_POSIX)
    return proc


async def spawn_exec(program: str, *args: Any, **kwargs: Any) -> Any:
    """`create_subprocess_exec` in its own process group, PGID recorded."""
    proc = await asyncio.create_subprocess_exec(
        program, *args, **{**subprocess_kwargs(), **kwargs}
    )
    record_process_group(proc, own_session=_POSIX)
    return proc


def spawn_popen(*popenargs: Any, **kwargs: Any) -> Any:
    """Synchronous ``Popen`` counterpart with the same group contract."""
    proc = subprocess.Popen(
        *popenargs, **{**subprocess_kwargs(), **kwargs},
    )
    record_process_group(proc, own_session=_POSIX)
    return proc


def record_process_group(proc: Any, *, own_session: bool) -> None:
    """Stamp the child's PGID onto `proc` so it survives the leader's death.

    `own_session` asserts the caller spawned this child with
    `subprocess_kwargs()`. It gates the one inference we cannot verify after the
    fact: when the child has already exited and been reaped, `getpgid` fails and
    the only remaining evidence that its group was its own pid is the spawn
    contract. Without that assertion we refuse to record rather than risk
    stamping a pgid that is really the *agent's* group.
    """
    if not _POSIX:
        return
    pid = getattr(proc, "pid", None)
    if not isinstance(pid, int) or pid <= 0:
        return  # test stub / already-closed transport — nothing safe to record
    try:
        pgid = os.getpgid(pid)
    except Exception:
        # Reaped between spawn and here (a fast `true`), or a stub object. The
        # spawn contract is the only evidence left.
        if not own_session:
            return
        pgid = pid
    if pgid != pid:
        # setsid() did not take effect, so this child shares OUR process group.
        # Recording it would make terminate_tree signal the whole agent.
        return
    try:
        setattr(proc, _PGID_ATTR, pgid)
    except Exception as e:  # pragma: no cover - exotic Process implementations
        logger.debug("could not record pgid for pid {}: {}", pid, e)


def process_group_alive(proc: Any) -> bool:
    """True while any process remains in `proc`'s group.

    Answers the question a `returncode` cannot: whether the *tree* is still
    running. A shell that backgrounds work exits with 0 straight away, so its
    returncode reports "done" while the real work continues in the same group.
    Callers that reclaim per-child bookkeeping must consult this or they will
    drop the only handle capable of stopping that work.

    False when the group id is unknown (non-POSIX, or a child we could not
    safely attribute a group to) — callers then fall back to returncode alone,
    which is the pre-existing behaviour.
    """
    pgid = _trusted_pgid(proc)
    if pgid is None:
        return False
    return _group_has_members(pgid)


async def terminate_tree(proc: Any, *, grace: float = 5.0) -> None:
    """Terminate `proc` and its whole process group, then reap it.

    Escalation mirrors a graceful shutdown: SIGTERM the group, wait up to
    `grace` seconds, then SIGKILL the group. The direct child is always
    awaited so it cannot survive as a zombie.

    Returns only once the group is empty (or `grace` has been spent on it a
    second time). An already-exited leader is NOT a shortcut: its group may
    still hold backgrounded grandchildren, and skipping the sweep there was how
    orphans escaped. Safe and cheap to call more than once — on an empty group
    the probe fails with ESRCH immediately.
    """
    if proc.returncode is None:
        _signal_group(proc, signal.SIGTERM)
        try:
            await asyncio.wait_for(proc.wait(), timeout=grace)
        except asyncio.TimeoutError:
            _signal_group(proc, signal.SIGKILL)
            try:
                await proc.wait()
            except Exception as e:  # pragma: no cover - defensive
                logger.debug("wait() after SIGKILL failed for pid {}: {}", proc.pid, e)
    await _sweep_group(proc, grace=grace)


async def _shield_cleanup_to_completion(
    proc: Any, *, grace: float,
) -> tuple[asyncio.CancelledError | None, BaseException | None]:
    """Run tree cleanup to completion despite cancellation of its caller.

    ``asyncio.shield`` alone is not sufficient: it keeps the cleanup task alive,
    but immediately raises ``CancelledError`` in the owner, which can then lose
    the task reference and return while descendants are still running. Keep the
    reference and await it again until it has genuinely settled. The first
    cancellation is returned to the caller so public cancellation semantics are
    preserved after cleanup.
    """
    cleanup = asyncio.create_task(terminate_tree(proc, grace=grace))
    delayed_cancel: asyncio.CancelledError | None = None
    while not cleanup.done():
        try:
            await asyncio.shield(cleanup)
        except asyncio.CancelledError as e:
            # A second Ctrl-C / shutdown cancellation must not punch through the
            # final lifecycle boundary. Remember it, then keep owning cleanup.
            delayed_cancel = delayed_cancel or e
        except BaseException:
            # Read and return the exact cleanup exception via task.result below.
            break

    try:
        cleanup.result()
    except BaseException as e:
        return delayed_cancel, e
    return delayed_cancel, None


async def communicate_owned(
    proc: Any,
    input: bytes | None = None,
    *,
    timeout: float | None = None,
    grace: float = 5.0,
) -> tuple[bytes, bytes]:
    """Communicate with a one-shot child, then reclaim its entire process tree.

    This is deliberately stronger than ``Process.communicate``: a launcher may
    exit successfully while a backgrounded descendant closes stdio and keeps
    running. Every one-shot caller must therefore sweep the recorded PGID on
    *all* exits — success, non-zero status, timeout, cancellation, or an arbitrary
    pipe/protocol failure. Long-lived owners such as ProcessTool and MCP stdio
    must not use this helper; they retain the process until their own stop/close.

    The operation's original result or exception wins over a cleanup failure.
    If communication succeeded but cleanup itself fails, that cleanup error is
    raised because the one-shot ownership contract was not fulfilled.
    """
    result: tuple[bytes, bytes] | None = None
    operation_error: BaseException | None = None
    operation_traceback = None
    try:
        # Preserve ``Process.communicate``'s no-argument contract for callers
        # without stdin.  Besides matching asyncio itself, this matters for
        # process doubles and alternate implementations whose method does not
        # accept an explicit ``None`` positional argument.
        communication = proc.communicate() if input is None else proc.communicate(input)
        if timeout is None:
            result = await communication
        else:
            result = await asyncio.wait_for(communication, timeout=timeout)
    except BaseException as e:
        operation_error = e
        operation_traceback = e.__traceback__

    delayed_cancel, cleanup_error = await _shield_cleanup_to_completion(
        proc, grace=grace,
    )

    if operation_error is not None:
        if cleanup_error is not None:
            logger.warning(
                "subprocess cleanup failed while preserving {}: {}",
                type(operation_error).__name__, cleanup_error,
            )
        raise operation_error.with_traceback(operation_traceback)
    if delayed_cancel is not None:
        if cleanup_error is not None:
            logger.warning(
                "subprocess cleanup failed while cancellation was delayed: {}",
                cleanup_error,
            )
        raise delayed_cancel
    if cleanup_error is not None:
        raise cleanup_error
    # ``result`` is assigned on the only path that reaches here.
    assert result is not None
    return result


def run_owned(
    *popenargs: Any,
    input: Any = None,
    capture_output: bool = False,
    timeout: float | None = None,
    check: bool = False,
    cleanup_grace: float = 5.0,
    **kwargs: Any,
) -> subprocess.CompletedProcess[Any]:
    """Tree-owning counterpart to :func:`subprocess.run`.

    The public behaviour matches the stdlib API used by this project:
    ``input``/``stdin`` validation, ``capture_output``, text or byte streams,
    timeout propagation, ``check=True`` and ``CompletedProcess`` results. The
    difference is lifecycle ownership: ``Popen`` starts a new session and its
    recorded process group is swept after every terminal path, including a
    successful or non-zero launcher exit, timeout, and arbitrary ``BaseException``.

    This helper owns only the *local* process tree. A service started through
    launchctl/systemctl, a container workload started through Docker, or a
    command daemonized on an SSH peer is owned by that external manager and is
    intentionally outside the local PGID.
    """
    if input is not None:
        if kwargs.get("stdin") is not None:
            raise ValueError("stdin and input arguments may not both be used.")
        kwargs["stdin"] = subprocess.PIPE

    if capture_output:
        if kwargs.get("stdout") is not None or kwargs.get("stderr") is not None:
            raise ValueError(
                "stdout and stderr arguments may not be used with capture_output."
            )
        kwargs["stdout"] = subprocess.PIPE
        kwargs["stderr"] = subprocess.PIPE

    proc = spawn_popen(*popenargs, **kwargs)
    stdout: Any = None
    stderr: Any = None
    operation_error: BaseException | None = None
    operation_traceback = None
    try:
        stdout, stderr = proc.communicate(input, timeout=timeout)
    except BaseException as e:
        operation_error = e
        operation_traceback = e.__traceback__

    cleanup_error: BaseException | None = None
    try:
        terminate_tree_sync(proc, grace=cleanup_grace)
        # subprocess.run completes the second communicate on Windows so the
        # TimeoutExpired instance contains accumulated pipe output. POSIX puts
        # partial output on the exception before raising already.
        if isinstance(operation_error, subprocess.TimeoutExpired) and not _POSIX:
            try:
                operation_error.stdout, operation_error.stderr = proc.communicate()
            except BaseException as e:  # original timeout remains authoritative
                logger.debug("post-timeout pipe collection failed: {}", e)
    except BaseException as e:
        cleanup_error = e
    finally:
        pipe_error = _close_process_pipes_sync(proc)
        cleanup_error = cleanup_error or pipe_error

    if operation_error is not None:
        if cleanup_error is not None:
            logger.warning(
                "synchronous subprocess cleanup failed while preserving {}: {}",
                type(operation_error).__name__, cleanup_error,
            )
        raise operation_error.with_traceback(operation_traceback)
    if cleanup_error is not None:
        raise cleanup_error

    return_code = proc.poll()
    if check and return_code:
        raise subprocess.CalledProcessError(
            return_code, proc.args, output=stdout, stderr=stderr,
        )
    return subprocess.CompletedProcess(proc.args, return_code, stdout, stderr)


def _close_process_pipes_sync(proc: Any) -> BaseException | None:
    """Close Popen streams like its context manager, returning the first error."""
    first_error: BaseException | None = None
    for name in ("stdout", "stderr", "stdin"):
        stream = getattr(proc, name, None)
        if stream is None:
            continue
        try:
            stream.close()
        except BaseException as e:  # preserve the operation error, if any
            first_error = first_error or e
    return first_error


def terminate_tree_sync(proc: Any, *, grace: float = 5.0) -> None:
    """Synchronous process-group termination for blocking CLI workflows.

    Mirrors :func:`terminate_tree`: TERM the isolated group, escalate to KILL,
    reap the direct child, then sweep descendants that outlived their leader.
    """
    if getattr(proc, "returncode", None) is None:
        _signal_group(proc, signal.SIGTERM)
        try:
            proc.wait(timeout=max(0.0, grace))
        except subprocess.TimeoutExpired:
            _signal_group(proc, signal.SIGKILL)
            try:
                proc.wait()
            except Exception as e:  # pragma: no cover - defensive
                logger.debug(
                    "sync wait() after SIGKILL failed for pid {}: {}",
                    getattr(proc, "pid", None), e,
                )
    _sweep_group_sync(proc, grace=grace)


async def _sweep_group(proc: Any, *, grace: float) -> None:
    """Drive the leftover group to empty: SIGTERM, poll, then SIGKILL.

    Members here are orphans, not our children, so `wait()` cannot be used —
    the loop probes existence instead. No-op unless we hold a trusted PGID and
    the group still has members, which makes this free on the common path.
    """
    pgid = _trusted_pgid(proc)
    if pgid is None or not _group_has_members(pgid):
        return
    _killpg(pgid, signal.SIGTERM)
    if await _wait_group_empty(pgid, timeout=grace):
        return
    _killpg(pgid, signal.SIGKILL)
    # A second window: SIGKILL is not instantaneous from the signaller's view.
    if not await _wait_group_empty(pgid, timeout=grace):
        logger.warning("process group {} still has members after SIGKILL", pgid)


def _sweep_group_sync(proc: Any, *, grace: float) -> None:
    """Blocking counterpart to :func:`_sweep_group`."""
    pgid = _trusted_pgid(proc)
    if pgid is None or not _group_has_members(pgid):
        return
    _killpg(pgid, signal.SIGTERM)
    if _wait_group_empty_sync(pgid, timeout=grace):
        return
    _killpg(pgid, signal.SIGKILL)
    if not _wait_group_empty_sync(pgid, timeout=grace):
        logger.warning("process group {} still has members after sync SIGKILL", pgid)


async def _wait_group_empty(pgid: int, *, timeout: float) -> bool:
    """Poll until the group is empty; True if it emptied within `timeout`."""
    deadline = asyncio.get_running_loop().time() + max(0.0, timeout)
    while True:
        if not _group_has_members(pgid):
            return True
        if asyncio.get_running_loop().time() >= deadline:
            return False
        await asyncio.sleep(_GROUP_POLL_INTERVAL)


def _wait_group_empty_sync(pgid: int, *, timeout: float) -> bool:
    deadline = time.monotonic() + max(0.0, timeout)
    while True:
        if not _group_has_members(pgid):
            return True
        if time.monotonic() >= deadline:
            return False
        time.sleep(_GROUP_POLL_INTERVAL)


def _trusted_pgid(proc: Any) -> int | None:
    """The PGID we may safely signal as a unit, or None.

    Prefers the value recorded at spawn (valid for the tree's whole life). Falls
    back to a live lookup for children spawned outside this module, keeping the
    original "only if the child is verified to be its own group leader" guard.
    Either way the agent's own group is never returned.
    """
    if not _POSIX:
        return None
    recorded = getattr(proc, _PGID_ATTR, None)
    if isinstance(recorded, int) and recorded > 0:
        return recorded if not _is_own_group(recorded) else None
    pid = getattr(proc, "pid", None)
    if not isinstance(pid, int) or pid <= 0:
        return None
    try:
        pgid = os.getpgid(pid)
    except Exception:
        return None
    if pgid != pid or _is_own_group(pgid):
        return None
    return pgid


def _is_own_group(pgid: int) -> bool:
    """Last-resort guard: never signal the group the agent itself lives in."""
    try:
        return pgid == os.getpgrp()
    except Exception:  # pragma: no cover - getpgrp does not fail on POSIX
        return True


def _group_has_members(pgid: int) -> bool:
    """Existence probe for a process group (signal 0 delivers nothing)."""
    try:
        os.killpg(pgid, 0)
        return True
    except ProcessLookupError:
        return False
    except PermissionError:
        # The group exists but is not ours to signal — we could not reap it
        # anyway, so report empty rather than spin until the grace expires.
        return False
    except Exception as e:  # pragma: no cover - defensive
        logger.debug("killpg({}, 0) probe failed: {}", pgid, e)
        return False


def _killpg(pgid: int, sig: int) -> None:
    try:
        os.killpg(pgid, sig)
    except ProcessLookupError:
        # The process group exited between the liveness probe and signal; that
        # is already the desired postcondition for termination.
        pass
    except Exception as e:  # pragma: no cover - defensive
        logger.debug("killpg({}, {}) failed: {}", pgid, sig, e)


def _signal_group(proc: Any, sig: int) -> None:
    """Signal the child's whole process group, falling back to the child alone.

    killpg is only used against a PGID `_trusted_pgid` vouched for — recorded at
    spawn, or verified live as the child's own group. That guard is critical: a
    child that is NOT a group leader shares the agent's group, and killpg would
    signal us. When no group can be trusted we signal just the direct child.
    """
    pgid = _trusted_pgid(proc)
    if pgid is not None:
        _killpg(pgid, sig)
        return
    try:
        if sig == signal.SIGKILL:
            proc.kill()
        else:
            proc.terminate()
    except ProcessLookupError:
        # The child exited between inspection and signaling, so no cleanup work
        # remains and termination can safely be treated as successful.
        pass
    except Exception as e:  # pragma: no cover - defensive
        logger.debug("single-process signal for pid {} failed: {}", getattr(proc, "pid", None), e)
