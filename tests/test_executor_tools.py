from __future__ import annotations

import asyncio
import os
import shlex
import signal
import sys
from types import SimpleNamespace

import pytest

from echo_agent.agent.executors.base import BaseExecutor, ExecRequest, ExecResponse, LocalExecutor
from echo_agent.agent.proc_lifecycle import _POSIX
from echo_agent.agent.tools.code_exec import CodeExecTool
from echo_agent.agent.tools.shell import ShellTool
from echo_agent.tools import ToolExecutionContext


_DETACHED_BACKGROUND_CMD = "sleep 90 >/dev/null 2>&1 & echo $!"


async def _assert_pid_gone(pid: int) -> None:
    for _ in range(50):
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            return
        await asyncio.sleep(0.05)
    pytest.fail(f"background process {pid} survived one-shot tool completion")


class RecordingExecutor(BaseExecutor):
    name = "recording"

    def __init__(self):
        self.requests: list[ExecRequest] = []

    async def setup(self) -> None:
        pass

    async def teardown(self) -> None:
        pass

    async def execute(self, request: ExecRequest) -> ExecResponse:
        self.requests.append(request)
        return ExecResponse(success=True, stdout=request.stdin or request.command, executor=self.name)


@pytest.mark.asyncio
async def test_shell_tool_uses_configured_executor(tmp_path) -> None:
    executor = RecordingExecutor()
    tool = ShellTool(str(tmp_path), executor=executor)

    result = await tool.execute({"command": "echo hello", "timeout": 3})

    assert result.success
    assert result.metadata["executor"] == "recording"
    assert executor.requests[0].command == "echo hello"


@pytest.mark.asyncio
@pytest.mark.skipif(not _POSIX, reason="process-tree cancellation is POSIX-only")
@pytest.mark.parametrize("configured_executor", [False, True])
async def test_shell_cancel_reaps_process_tree(tmp_path, configured_executor) -> None:
    """Both the direct fallback and production LocalExecutor must reap on cancel."""
    marker = tmp_path / f"ticks-{configured_executor}.txt"
    pid_file = tmp_path / f"pid-{configured_executor}.txt"
    script = (
        "import os,time\n"
        f"open({str(pid_file)!r},'w').write(str(os.getpid()))\n"
        f"p={str(marker)!r}\n"
        "for _ in range(200):\n"
        " f=open(p,'a'); f.write('x'); f.close(); time.sleep(.05)\n"
    )
    # Use a script file rather than ``python -c``: the shell policy correctly
    # treats inline interpreters as approval-requiring, which is unrelated to
    # the subprocess cancellation behavior under test.
    script_file = tmp_path / f"writer-{configured_executor}.py"
    script_file.write_text(script)
    command = f"{shlex.quote(sys.executable)} {shlex.quote(str(script_file))}"
    executor = LocalExecutor(str(tmp_path)) if configured_executor else None
    tool = ShellTool(str(tmp_path), executor=executor)
    task = asyncio.create_task(tool.execute({"command": command, "timeout": 60}))

    try:
        for _ in range(80):
            await asyncio.sleep(0.05)
            if marker.exists() and marker.stat().st_size:
                break
        assert marker.exists(), "shell child never started"

        task.cancel()
        with pytest.raises(asyncio.CancelledError):
            await task

        size_after_cancel = marker.stat().st_size
        await asyncio.sleep(0.6)
        assert marker.stat().st_size == size_after_cancel
    finally:
        if not task.done():
            task.cancel()
            await asyncio.gather(task, return_exceptions=True)
        if pid_file.exists():
            try:
                os.kill(int(pid_file.read_text()), signal.SIGKILL)
            except (ProcessLookupError, ValueError):
                pass


@pytest.mark.asyncio
@pytest.mark.skipif(not _POSIX, reason="process-tree assertion is POSIX-only")
@pytest.mark.parametrize("configured_executor", [False, True])
async def test_shell_success_reaps_detached_background_work(
    tmp_path, configured_executor,
) -> None:
    """Successful one-shot shell calls cannot become an implicit daemon API."""
    executor = LocalExecutor(str(tmp_path)) if configured_executor else None
    tool = ShellTool(str(tmp_path), executor=executor)
    child_pid = 0
    try:
        result = await tool.execute({
            "command": _DETACHED_BACKGROUND_CMD,
            "timeout": 5,
        })
        assert result.success, result.error
        child_pid = int(result.output.strip())
        await _assert_pid_gone(child_pid)
    finally:
        if child_pid:
            try:
                os.kill(child_pid, signal.SIGKILL)
            except ProcessLookupError:
                pass


@pytest.mark.asyncio
async def test_code_exec_uses_stdin_and_language_allowlist(tmp_path) -> None:
    executor = RecordingExecutor()
    tool = CodeExecTool(str(tmp_path), executor=executor, allowed_languages=["python"])

    denied = await tool.execute({"language": "bash", "code": "echo no"})
    allowed = await tool.execute({"language": "python", "code": "print('ok')"})

    assert not denied.success
    assert "Language not allowed" in denied.error
    assert allowed.success
    assert executor.requests[0].command == "python3 -"
    assert executor.requests[0].stdin == "print('ok')"


@pytest.mark.asyncio
@pytest.mark.skipif(not _POSIX, reason="process-tree assertion is POSIX-only")
async def test_code_exec_success_reaps_detached_background_work(tmp_path) -> None:
    tool = CodeExecTool(
        str(tmp_path),
        allowed_languages=["python"],
        exec_policy=SimpleNamespace(security="full", ask="on_miss"),
    )
    code = (
        "import subprocess\n"
        "child = subprocess.Popen(\n"
        "    ['sleep', '90'],\n"
        "    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,\n"
        ")\n"
        "print(child.pid, flush=True)\n"
    )
    ctx = ToolExecutionContext(
        approved_actions=frozenset({"dynamic_execution"}),
    )
    child_pid = 0
    try:
        result = await tool.execute({
            "language": "python", "code": code, "timeout": 5,
        }, ctx)
        assert result.success, result.error
        child_pid = int(result.output.strip())
        await _assert_pid_gone(child_pid)
    finally:
        if child_pid:
            try:
                os.kill(child_pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
