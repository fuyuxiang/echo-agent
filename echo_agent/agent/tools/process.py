"""Process management tool — start, list, poll, and stop background processes."""

from __future__ import annotations

import asyncio
import time
from types import SimpleNamespace
from typing import Any

from loguru import logger

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
        )
        pid = f"proc_{proc.pid}"
        self._processes[pid] = {
            "process": proc,
            "command": cmd,
            "started": time.time(),
            "stdout_buf": b"",
            "stderr_buf": b"",
        }

        # Keep a strong reference in the registry — a bare create_task result
        # can be garbage-collected mid-execution.
        collector = asyncio.create_task(self._collect_output(pid))
        collector.add_done_callback(
            lambda t: not t.cancelled() and t.exception()
            and logger.warning("Output collector for {} failed: {}", pid, t.exception())
        )
        self._processes[pid]["collector"] = collector
        return ToolResult(output=f"Started process {pid}: {cmd}", metadata={"process_id": pid})

    def _list(self) -> ToolResult:
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
        if proc.returncode is None:
            proc.terminate()
            try:
                await asyncio.wait_for(proc.wait(), timeout=5)
            except asyncio.TimeoutError:
                proc.kill()
                await proc.wait()
        collector = info.get("collector")
        if collector is not None:
            collector.cancel()
        del self._processes[pid]
        return ToolResult(output=f"Stopped {pid}")

    async def aclose(self) -> None:
        """Terminate every live child process and cancel its collector.

        Called from AgentLoop.stop() so background processes do not outlive the
        agent. Kept idempotent — a second call over an empty table is a no-op.
        """
        for pid, info in list(self._processes.items()):
            proc = info["process"]
            if proc.returncode is None:
                proc.terminate()
                try:
                    await asyncio.wait_for(proc.wait(), timeout=5)
                except asyncio.TimeoutError:
                    proc.kill()
                    await proc.wait()
            for key in ("collector", "watchdog"):
                task = info.get(key)
                if task is not None:
                    task.cancel()
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
            await asyncio.gather(
                _drain(proc.stdout, "stdout_buf"),
                _drain(proc.stderr, "stderr_buf"),
            )
        except Exception as e:
            logger.debug("Error reading process output: {}", e)
