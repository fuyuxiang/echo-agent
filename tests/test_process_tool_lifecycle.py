"""ProcessTool lifecycle contract: per-instance table, stderr drain, timeout, aclose."""

from __future__ import annotations

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
