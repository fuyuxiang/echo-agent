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


@pytest.mark.asyncio
async def test_interrupted_returns_notice():
    mgr = ClarifyManager()
    tool = ClarifyTool(manager=mgr)
    req = mgr.request("q", ["A"], session_key="s1")
    ctx = ToolExecutionContext(channel="gateway:cli", user_id="u1", session_key="s1")
    params = {"question": "q", "options": ["A"], "_clarify_id": req.id}

    async def cancel():
        await asyncio.sleep(0.01)
        mgr.cancel_session("s1")

    t = asyncio.create_task(cancel())
    result = await tool.execute(params, ctx)
    await t
    assert result.success is True
    assert "会话中断" in result.output


@pytest.mark.asyncio
async def test_empty_user_answer_is_not_interrupt():
    mgr = ClarifyManager()
    tool = ClarifyTool(manager=mgr)
    req = mgr.request("q", ["A"], session_key="s1")
    ctx = ToolExecutionContext(channel="gateway:cli", user_id="u1", session_key="s1")
    params = {"question": "q", "options": ["A"], "_clarify_id": req.id}

    async def ans():
        await asyncio.sleep(0.01)
        mgr.resolve(req.id, "")

    t = asyncio.create_task(ans())
    result = await tool.execute(params, ctx)
    await t
    assert result.success is True
    assert "会话中断" not in result.output   # 答空不再被误报为中断
