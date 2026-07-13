import asyncio
import pytest

from echo_agent.agent.clarify_manager import ClarifyManager
from echo_agent.agent.tools.clarify import ClarifyTool
from echo_agent.tools.base import ToolExecutionContext


@pytest.mark.asyncio
async def test_cli_channel_blocks_until_resolved():
    mgr = ClarifyManager()
    tool = ClarifyTool(manager=mgr)
    # pipeline pre-registers and injects the id:
    req = mgr.request("选哪个?", ["A", "B"], user_id="u1")
    ctx = ToolExecutionContext(channel="gateway:cli", user_id="u1")
    params = {"question": "选哪个?", "options": ["A", "B"], "_clarify_id": req.id}

    async def answer_later():
        await asyncio.sleep(0.01)
        mgr.resolve(req.id, "A")

    task = asyncio.create_task(answer_later())
    result = await tool.execute(params, ctx)
    await task
    assert result.success is True
    assert "A" in result.output


@pytest.mark.asyncio
async def test_non_cli_channel_returns_text_without_blocking():
    mgr = ClarifyManager()
    tool = ClarifyTool(manager=mgr)
    ctx = ToolExecutionContext(channel="wecom", user_id="u1")
    params = {"question": "选哪个?", "options": ["A", "B"]}
    # Must return immediately (no resolve ever happens).
    result = await asyncio.wait_for(tool.execute(params, ctx), timeout=1.0)
    assert result.success is True
    assert "选哪个?" in result.output
    assert "1. A" in result.output


@pytest.mark.asyncio
async def test_cli_without_injected_id_self_registers():
    mgr = ClarifyManager()
    tool = ClarifyTool(manager=mgr)
    ctx = ToolExecutionContext(channel="gateway:cli", user_id="u1")
    params = {"question": "q", "options": ["X"]}

    async def answer_when_pending():
        for _ in range(100):
            ids = list(mgr._pending.keys())
            if ids:
                mgr.resolve(ids[0], "X")
                return
            await asyncio.sleep(0.005)

    task = asyncio.create_task(answer_when_pending())
    result = await tool.execute(params, ctx)
    await task
    assert "X" in result.output


def test_timeout_seconds_is_large():
    # registry wraps execute() in asyncio.wait_for(timeout=tool.timeout_seconds);
    # a CLI clarify may wait indefinitely, so the ceiling must be very high.
    assert ClarifyTool.timeout_seconds >= 3600
