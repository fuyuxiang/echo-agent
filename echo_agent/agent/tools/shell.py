"""Shell execution tool — runs commands with isolation and safety controls."""

from __future__ import annotations

import asyncio
import os
from typing import Any

from echo_agent.agent.tools.base import Tool, ToolExecutionContext, ToolPermission, ToolResult


class ShellTool(Tool):
    name = "exec"
    description = "Execute a shell command in the workspace."
    parameters = {
        "type": "object",
        "properties": {
            "command": {"type": "string", "description": "The shell command to execute."},
            "timeout": {"type": "integer", "description": "Timeout in seconds.", "default": 30},
            "cwd": {"type": "string", "description": "Working directory override."},
        },
        "required": ["command"],
    }
    required_permissions = [ToolPermission.EXECUTE]
    timeout_seconds = 60

    def __init__(self, workspace: str, allowed: list[str] | None = None, blocked: list[str] | None = None, max_output: int = 16000):
        self._workspace = workspace
        self._allowed = allowed or []
        self._blocked = blocked or []
        self._max_output = max_output

    def _check_command(self, command: str) -> str | None:
        cmd_name = command.strip().split()[0] if command.strip() else ""
        for pattern in self._blocked:
            if pattern in command:
                return f"Command blocked: contains '{pattern}'"
        if self._allowed and cmd_name not in self._allowed:
            return f"Command not in allowlist: {cmd_name}"
        return None

    async def execute(self, params: dict[str, Any], ctx: ToolExecutionContext | None = None) -> ToolResult:
        command = params["command"]
        timeout = params.get("timeout", 30)
        cwd = params.get("cwd", self._workspace)

        violation = self._check_command(command)
        if violation:
            return ToolResult(success=False, error=violation)

        try:
            proc = await asyncio.create_subprocess_shell(
                command,
                stdout=asyncio.subprocess.PIPE,
                stderr=asyncio.subprocess.PIPE,
                cwd=cwd,
                env={**os.environ, "WORKSPACE": self._workspace},
            )
            stdout, stderr = await asyncio.wait_for(proc.communicate(), timeout=timeout)
            output = stdout.decode(errors="replace")
            err_output = stderr.decode(errors="replace")

            if len(output) > self._max_output:
                output = output[:self._max_output] + f"\n... (truncated, {len(output)} total chars)"

            combined = output
            if err_output:
                combined += f"\nSTDERR:\n{err_output}"

            return ToolResult(
                success=proc.returncode == 0,
                output=combined,
                error=err_output if proc.returncode != 0 else "",
                metadata={"return_code": proc.returncode},
            )
        except asyncio.TimeoutError:
            return ToolResult(success=False, error=f"Command timed out after {timeout}s")
        except Exception as e:
            return ToolResult(success=False, error=str(e))

    def execution_mode(self, params: dict[str, Any]) -> str:
        return "side_effect"
