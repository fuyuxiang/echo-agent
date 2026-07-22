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


@pytest.mark.asyncio
async def test_timeout_kills_and_marks_process(tmp_path):
    tool = ProcessTool(str(tmp_path))
    pid = await _start(tool, "sleep 30", timeout=1)
    proc = tool._processes[pid]["process"]
    # Watchdog must fire ~1s in, terminate/kill the child, and mark it timed_out.
    await asyncio.wait_for(proc.wait(), timeout=10)
    assert proc.returncode is not None
    assert tool._processes[pid]["timed_out"] is True
    poll = await tool._poll(pid)
    assert "timed out" in poll.output.lower()
    await tool.aclose()


@pytest.mark.asyncio
async def test_aclose_terminates_processes_and_clears_table(tmp_path):
    tool = ProcessTool(str(tmp_path))
    pid = await _start(tool, "sleep 30")
    proc = tool._processes[pid]["process"]
    assert proc.returncode is None
    await tool.aclose()
    # Process killed and table emptied.
    assert proc.returncode is not None
    assert tool._processes == {}
    # Idempotent: a second aclose over an empty table must not raise.
    await tool.aclose()


@pytest.mark.asyncio
async def test_agent_stop_closes_process_tool(tmp_path, monkeypatch):
    from echo_agent.agent.loop import AgentLoop
    from echo_agent.config.loader import load_config
    from echo_agent.bus.queue import MessageBus
    from echo_agent.models.provider import LLMProvider, LLMResponse

    # A minimal real provider — AgentLoop.__init__ dereferences
    # provider.chat_with_retry (via MemoryConsolidator), so provider=None crashes
    # construction before stop() is ever reached.
    class _StubProvider(LLMProvider):
        async def chat(self, messages, tools=None, model=None, tool_choice=None, **kwargs):
            return LLMResponse(content="ok", finish_reason="stop")

        def get_default_model(self):
            return "stub"

    config = load_config(overrides={"workspace": str(tmp_path)})
    config.tools.exec.enabled = True
    loop = AgentLoop(bus=MessageBus(), config=config, provider=_StubProvider(), workspace=tmp_path)
    proc_tool = loop.tools.get("process")
    assert proc_tool is not None
    closed = {"n": 0}
    orig = proc_tool.aclose

    async def _spy():
        closed["n"] += 1
        await orig()

    monkeypatch.setattr(proc_tool, "aclose", _spy)
    await loop.stop()
    assert closed["n"] == 1
