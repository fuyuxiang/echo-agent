"""Shell execution tool — runs commands with isolation and safety controls."""

from __future__ import annotations

import asyncio
import os
import re
from types import SimpleNamespace
from pathlib import Path
from typing import Any

from echo_agent.agent.executors.base import BaseExecutor, ExecRequest, prepend_interpreter_bin
from echo_agent.agent.proc_lifecycle import spawn_shell, terminate_tree
from echo_agent.agent.tools.base import Tool, ToolExecutionContext, ToolResult
from echo_agent.security.guards import evaluate_shell_command
from echo_agent.security.path_policy import check_cwd


class ShellTool(Tool):
    name = "exec"
    description = "Execute a shell command in the workspace."
    risk_level = "exec"
    parameters = {
        "type": "object",
        "properties": {
            "command": {"type": "string", "description": "The shell command to execute."},
            "timeout": {"type": "integer", "description": "Timeout in seconds.", "default": 30},
            "cwd": {"type": "string", "description": "Working directory override."},
        },
        "required": ["command"],
    }
    timeout_seconds = 60
    _BUILTIN_BLOCKED_PATTERNS = [
        (re.compile(r"/etc/(passwd|shadow|sudoers|gshadow)\b"), "sensitive system account file"),
        (re.compile(r"(^|\s)(cat|less|more|head|tail|sed|awk|grep)\s+[^\n;|&]*(/root/\.ssh|/etc/ssh|/root/\.gnupg)"), "sensitive credential path"),
        (re.compile(r"\brm\s+-[^\n;|&]*[rf][^\n;|&]*\s+/(?:\s|$)"), "destructive root removal"),
        (re.compile(r"\bdd\s+[^\n;|&]*\bof=/dev/"), "destructive block device write"),
        (re.compile(r"\bmkfs(?:\.\w+)?\b"), "filesystem formatting"),
        (re.compile(r"\b(shutdown|reboot|halt|poweroff)\b"), "system shutdown"),
    ]

    def __init__(
        self,
        workspace: str,
        allowed: list[str] | None = None,
        blocked: list[str] | None = None,
        max_output: int = 2000000,
        executor: BaseExecutor | None = None,
        exec_policy: Any | None = None,
        network_policy: str = "allow",
    ):
        self._workspace = str(Path(workspace).resolve())
        self._allowed = allowed or []
        self._blocked = blocked or []
        self._max_output = max_output
        self._executor = executor
        self._exec_policy = exec_policy
        self._network_policy = network_policy

    def _bound(self, text: str) -> str:
        """套采集上限。stderr 此前完全没套,而 return_code != 0 时它就是模型
        读到的全部内容——构建失败、pytest 失败正是最常见的大输出场景。"""
        if len(text) <= self._max_output:
            return text
        return text[:self._max_output] + f"\n... (truncated, {len(text)} total chars)"

    def _check_command(self, command: str) -> str | None:
        cmd_name = command.strip().split()[0] if command.strip() else ""
        for pattern, reason in self._BUILTIN_BLOCKED_PATTERNS:
            if pattern.search(command):
                return f"Command blocked by safety policy: {reason}"
        for pattern in self._blocked:
            if pattern in command:
                return f"Command blocked: contains '{pattern}'"
        if self._allowed and cmd_name not in self._allowed:
            return f"Command not in allowlist: {cmd_name}"
        return None

    def _policy_violation(self, command: str, ctx: ToolExecutionContext | None) -> str | None:
        policy = self._exec_policy
        if policy is None:
            policy = SimpleNamespace(
                security="full" if not self._allowed else "allowlist",
                ask="off",
                allowed_commands=self._allowed,
                blocked_commands=self._blocked,
                safe_bins=[],
            )
        decision = evaluate_shell_command(
            command,
            exec_policy=policy,
            network_policy=self._network_policy,
            approval_action=self.name,
        )
        if decision.action == "allow":
            return None
        approved = ctx and (self.name in ctx.approved_actions or decision.pattern_key in ctx.approved_actions)
        if decision.action == "ask" and approved:
            return None
        return f"Command blocked by execution policy: {decision.reason}"

    def _resolve_cwd(self, cwd: str) -> str:
        raw = Path(cwd).expanduser()
        resolved = raw.resolve() if raw.is_absolute() else (Path(self._workspace) / raw).resolve()
        violation = check_cwd(str(resolved))
        if violation:
            raise ValueError(violation)
        return str(resolved)

    async def execute(self, params: dict[str, Any], ctx: ToolExecutionContext | None = None) -> ToolResult:
        command = params["command"]
        timeout = params.get("timeout", 30)
        cwd = params.get("cwd", self._workspace)

        violation = self._check_command(command)
        if violation:
            return ToolResult(success=False, error=violation)
        policy_violation = self._policy_violation(command, ctx)
        if policy_violation:
            return ToolResult(success=False, error=policy_violation)

        proc = None
        try:
            try:
                cwd = self._resolve_cwd(cwd)
            except ValueError:
                return ToolResult(success=False, error=f"cwd is outside workspace: {cwd}")
            if self._executor:
                response = await self._executor.execute(ExecRequest(
                    command=command,
                    cwd=cwd,
                    timeout=timeout,
                    env={"WORKSPACE": self._workspace},
                    credentials=ctx.credentials if ctx else {},
                ))
                output = response.stdout
                err_output = response.stderr
                return_code = response.return_code
                executor_name = response.executor
            else:
                proc = await spawn_shell(
                    command,
                    stdout=asyncio.subprocess.PIPE,
                    stderr=asyncio.subprocess.PIPE,
                    cwd=cwd,
                    env={**prepend_interpreter_bin(dict(os.environ)), "WORKSPACE": self._workspace},
                )
                stdout, stderr = await asyncio.wait_for(proc.communicate(), timeout=timeout)
                output = stdout.decode(errors="replace")
                err_output = stderr.decode(errors="replace")
                return_code = proc.returncode or 0
                executor_name = "direct"

            output = self._bound(output)
            err_output = self._bound(err_output)

            combined = output
            if err_output:
                combined += f"\nSTDERR:\n{err_output}"
            # combined 是模型实际读到的那一份,必须自己也受上限约束:stdout 与
            # stderr 各自贴着上限时,合起来正好是两倍,采集上限就名不副实了。
            combined = self._bound(combined)

            return ToolResult(
                success=return_code == 0,
                output=combined,
                error=err_output if return_code != 0 else "",
                metadata={"return_code": return_code, "executor": executor_name},
            )
        except asyncio.TimeoutError:
            if proc is not None:
                await terminate_tree(proc)
            return ToolResult(success=False, error=f"Command timed out after {timeout}s")
        except Exception as e:
            if proc is not None:
                # Not gated on `returncode is None`: an exited leader can still
                # have left backgrounded grandchildren in its group, and this
                # tool holds the only handle to them.
                await terminate_tree(proc)
            return ToolResult(success=False, error=str(e))

    def execution_mode(self, params: dict[str, Any]) -> str:
        return "side_effect"
