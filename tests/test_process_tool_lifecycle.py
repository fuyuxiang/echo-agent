"""ProcessTool lifecycle contract: per-instance table, stderr drain, timeout, aclose."""

from __future__ import annotations

import asyncio

import pytest

from echo_agent.agent.tools.process import ProcessTool


async def _start(tool: ProcessTool, command: str, *, timeout: int = 300) -> str:
    result = await tool.execute({"action": "start", "command": command, "timeout": timeout})
    assert result.success, result.error
    return result.metadata["process_id"]


@pytest.mark.asyncio
async def test_process_table_is_per_instance(tmp_path):
    tool_a = ProcessTool(str(tmp_path))
    tool_b = ProcessTool(str(tmp_path))
    pid = await _start(tool_a, "true")
    # tool_b must not see tool_a's process — no module-level cross-talk.
    listing = tool_b._list()
    assert pid not in listing.output
    assert "No background processes." in listing.output
    await tool_a.aclose()
    await tool_b.aclose()


@pytest.mark.asyncio
async def test_heavy_stderr_does_not_deadlock(tmp_path):
    tool = ProcessTool(str(tmp_path))
    # Write a lot of data to stderr AND stdout. If the collector only drains
    # stdout, the stderr pipe buffer (~64KB) fills and the child blocks forever.
    # Run a script file rather than `python3 -c ...`: the inline-interpreter
    # form is hard-denied by the exec policy before the subprocess ever starts.
    script = tmp_path / "heavy_stderr.py"
    script.write_text(
        "import sys\n"
        "sys.stderr.write('E' * 200000)\n"
        "sys.stdout.write('O' * 10000)\n"
        "sys.stderr.flush()\n"
        "sys.stdout.flush()\n"
    )
    cmd = f"python3 {script}"
    pid = await _start(tool, cmd)
    proc = tool._processes[pid]["process"]
    # Must terminate on its own; a deadlocked child never exits.
    await asyncio.wait_for(proc.wait(), timeout=10)
    # Give the collector a beat to flush the final chunks.
    collector = tool._processes[pid]["collector"]
    await asyncio.wait_for(collector, timeout=5)
    poll = await tool._poll(pid)
    assert "exited(0)" in poll.output
    assert "[stderr]" in poll.output  # stderr_buf was actually populated
    # Ring truncation kept stderr bounded.
    assert len(tool._processes[pid]["stderr_buf"]) <= 100_000
    await tool.aclose()
