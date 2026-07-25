"""Process management tool — start, list, poll, and stop background processes."""

from __future__ import annotations

import asyncio
import time
from types import SimpleNamespace
from typing import Any

from loguru import logger

from echo_agent.agent.proc_lifecycle import subprocess_kwargs, terminate_tree
from echo_agent.agent.tools.base import Tool, ToolExecutionContext, ToolResult
from echo_agent.security.guards import evaluate_shell_command


class ProcessTool(Tool):
    name = "process"
    description = "Manage background processes: start, list, poll output, or stop."
    risk_level = "exec"
    parameters = {
        "type": "object",
        "properties": {
            "action": {"type": "string", "enum": ["start", "list", "poll", "stop"], "description": "Action to perform."},
            "command": {"type": "string", "description": "Shell command to run (for 'start')."},
            "process_id": {"type": "string", "description": "Process ID (for 'poll'/'stop')."},
            "timeout": {"type": "integer", "description": "Timeout in seconds for start.", "default": 300},
        },
        "required": ["action"],
    }
    timeout_seconds = 10

    # Retention for entries whose process has exited. They cannot be dropped the
    # moment the child dies — `poll` must still return the output and exit code —
    # but they must not accumulate either: each entry pins a Process object plus
    # up to 100KB of stdout and stderr buffers, so a long-lived agent that keeps
    # starting background commands would grow without bound. Live processes are
    # never reclaimed regardless of these limits.
    _EXITED_TTL_SECONDS = 1800.0
    _MAX_EXITED_ENTRIES = 64

    def __init__(self, workspace: str, *, exec_policy: Any | None = None, network_policy: str = "allow"):
        self._workspace = workspace
        self._exec_policy = exec_policy
        self._network_policy = network_policy
        # Per-instance process table — a module-level global would let one Agent
        # instance's ProcessTool see and stop another's background processes.
        self._processes: dict[str, dict[str, Any]] = {}

    async def execute(self, params: dict[str, Any], ctx: ToolExecutionContext | None = None) -> ToolResult:
        action = params["action"]

        if action == "start":
            return await self._start(params, ctx)
        elif action == "list":
            return self._list()
        elif action == "poll":
            return await self._poll(params.get("process_id", ""))
        elif action == "stop":
            return await self._stop(params.get("process_id", ""))
        return ToolResult(success=False, error=f"Unknown action: {action}")

    async def _start(self, params: dict[str, Any], ctx: ToolExecutionContext | None = None) -> ToolResult:
        cmd = params.get("command", "")
        if not cmd:
            return ToolResult(success=False, error="No command provided")

        policy = self._exec_policy or SimpleNamespace(security="full", ask="off")
        decision = evaluate_shell_command(
            cmd,
            exec_policy=policy,
            network_policy=self._network_policy,
            approval_action=self.name,
        )
        approved = ctx and (self.name in ctx.approved_actions or decision.pattern_key in ctx.approved_actions)
        if decision.action == "deny" or (decision.action == "ask" and not approved):
            return ToolResult(success=False, error=f"Process blocked by execution policy: {decision.reason}")

        proc = await asyncio.create_subprocess_shell(
            cmd,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
            cwd=self._workspace,
            **subprocess_kwargs(),
        )
        pid = f"proc_{proc.pid}"
        # Reclaim before inserting, so the cap counts entries already in the
        # table and the fresh one is never itself a candidate.
        self._reap_finished()
        self._processes[pid] = {
            "process": proc,
            "command": cmd,
            "started": time.time(),
            "stdout_buf": b"",
            "stderr_buf": b"",
            "timed_out": False,
            # Set by _collect_output once both pipes hit EOF; drives reclamation.
            "finished_at": None,
        }

        # Keep a strong reference in the registry — a bare create_task result
        # can be garbage-collected mid-execution.
        collector = asyncio.create_task(self._collect_output(pid))
        collector.add_done_callback(
            lambda t: not t.cancelled() and t.exception()
            and logger.warning("Output collector for {} failed: {}", pid, t.exception())
        )
        self._processes[pid]["collector"] = collector

        # Timeout watchdog: the `timeout` param was declared in the schema but
        # never read, so long-running commands ran unbounded. Arm a watchdog
        # that terminates (then kills) the child after `timeout` seconds.
        timeout = float(params.get("timeout", 300) or 300)
        watchdog = asyncio.create_task(self._watchdog(pid, timeout))
        watchdog.add_done_callback(
            lambda t: not t.cancelled() and t.exception()
            and logger.warning("Watchdog for {} failed: {}", pid, t.exception())
        )
        self._processes[pid]["watchdog"] = watchdog
        return ToolResult(output=f"Started process {pid}: {cmd}", metadata={"process_id": pid})

    def _reap_finished(self) -> None:
        """Drop bookkeeping for exited processes past their retention window.

        Entries are eligible only once the child has actually exited (returncode
        is not None); a running process is never reclaimed, however old. Called
        on every `start` so the table is bounded by use rather than by a timer.
        """
        now = time.time()
        exited = [
            (pid, info) for pid, info in self._processes.items()
            if info["process"].returncode is not None
        ]
        stale = {
            pid for pid, info in exited
            if now - (info.get("finished_at") or now) >= self._EXITED_TTL_SECONDS
        }
        # Oldest-first beyond the cap, so recent results stay pollable.
        overflow = len(exited) - self._MAX_EXITED_ENTRIES
        if overflow > 0:
            by_age = sorted(exited, key=lambda kv: kv[1].get("finished_at") or 0.0)
            stale.update(pid for pid, _ in by_age[:overflow])
        for pid in stale:
            self._processes.pop(pid, None)

    async def _watchdog(self, pid: str, timeout: float) -> None:
        """Kill a process that outlives its timeout, marking it timed_out.

        Reaps the whole process group (see terminate_tree) so pipeline and
        backgrounded grandchildren die too, not just the direct shell.
        Cancelled quietly when the process exits on its own (watchdog is
        cancelled in _stop/aclose).
        """
        info = self._processes.get(pid)
        if not info:
            return
        proc = info["process"]
        try:
            await asyncio.wait_for(proc.wait(), timeout=timeout)
            return  # exited before the deadline — nothing to do
        except asyncio.TimeoutError:
            pass
        info["timed_out"] = True
        await terminate_tree(proc)

    def _list(self) -> ToolResult:
        # Also reclaim here: a process that exits after the last `start` would
        # otherwise linger until the next one, and `list` is the natural point at
        # which a caller observes the table.
        self._reap_finished()
        if not self._processes:
            return ToolResult(output="No background processes.")
        lines = []
        for pid, info in self._processes.items():
            proc = info["process"]
            status = "running" if proc.returncode is None else f"exited({proc.returncode})"
            elapsed = int(time.time() - info["started"])
            lines.append(f"{pid}: [{status}] {elapsed}s — {info['command'][:80]}")
        return ToolResult(output="\n".join(lines))

    async def _poll(self, pid: str) -> ToolResult:
        if pid not in self._processes:
            return ToolResult(success=False, error=f"Process '{pid}' not found")
        info = self._processes[pid]
        proc = info["process"]
        status = "running" if proc.returncode is None else f"exited({proc.returncode})"
        if info.get("timed_out"):
            status += " timed out"
        stdout = info["stdout_buf"].decode(errors="replace")[-8000:]
        stderr = info["stderr_buf"].decode(errors="replace")[-4000:]
        output = f"[{status}]\n{stdout}"
        if stderr:
            output += f"\n[stderr]\n{stderr}"
        return ToolResult(output=output, metadata={"status": status})

    async def _stop(self, pid: str) -> ToolResult:
        if pid not in self._processes:
            return ToolResult(success=False, error=f"Process '{pid}' not found")
        info = self._processes[pid]
        proc = info["process"]
        await terminate_tree(proc)
        for key in ("collector", "watchdog"):
            task = info.get(key)
            if task is not None:
                task.cancel()
        del self._processes[pid]
        return ToolResult(output=f"Stopped {pid}")

    async def aclose(self) -> None:
        """Terminate every live child process and cancel its collectors.

        Called from AgentLoop.stop() so background processes do not outlive the
        agent. Idempotent — a second call over an empty table is a no-op.
        """
        for pid, info in list(self._processes.items()):
            proc = info["process"]
            await terminate_tree(proc)
            tasks = [info.get("collector"), info.get("watchdog")]
            for task in tasks:
                if task is not None:
                    task.cancel()
            # Await cancellation so no reader task survives the event loop.
            for task in tasks:
                if task is not None:
                    try:
                        await task
                    except (asyncio.CancelledError, Exception):
                        pass
        self._processes.clear()

    async def _collect_output(self, pid: str) -> None:
        info = self._processes.get(pid)
        if not info:
            return
        proc = info["process"]

        async def _drain(stream: Any, buf_key: str) -> None:
            if stream is None:
                return
            while True:
                chunk = await stream.read(4096)
                if not chunk:
                    break
                info[buf_key] += chunk
                if len(info[buf_key]) > 100_000:
                    info[buf_key] = info[buf_key][-50_000:]

        try:
            # Drain stdout AND stderr concurrently. Reading only stdout lets the
            # stderr pipe buffer fill (~64KB on Linux) and the child blocks on
            # its next stderr write forever — a classic subprocess deadlock.
            # return_exceptions keeps one stream's failure from cancelling the
            # other mid-drain, which would leave the child blocked on a full pipe.
            results = await asyncio.gather(
                _drain(proc.stdout, "stdout_buf"),
                _drain(proc.stderr, "stderr_buf"),
                return_exceptions=True,
            )
            for r in results:
                if isinstance(r, BaseException):
                    logger.debug("Error reading process output for {}: {}", pid, r)
        except Exception as e:
            logger.debug("Error reading process output: {}", e)
        finally:
            # Stamp the reclaim clock: both pipes are at EOF, so the child has
            # exited (or is about to be reaped). _reap_finished uses this to age
            # the entry out while keeping it pollable in the meantime.
            if info.get("finished_at") is None:
                info["finished_at"] = time.time()
