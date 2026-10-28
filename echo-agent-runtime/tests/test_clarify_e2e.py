"""End-to-end: pipeline registers a clarify, the TUI-equivalent /clarify reply
resolves it, and the blocked tool returns the user's answer to the model."""

import asyncio
import pytest

from echo_agent.agent.clarify_manager import ClarifyManager
from echo_agent.agent.tools.clarify import ClarifyTool
from echo_agent.tools import ToolExecutionContext


class _FakeCog:
    def __init__(self):
        self.emitted = []

    @staticmethod
    def active(event):
        return True

    async def emit(self, event, cog_type, data, summary):
        self.emitted.append((cog_type, data))


class _FakeToolCall:
    def __init__(self, name, arguments):
        self.name = name
        self.arguments = arguments
        self.id = "tc1"


class _FakeEvent:
    channel = "gateway:cli"
    chat_id = "c"
    event_id = "in_1"
    reply_to_id = ""
    sender_id = "u1"


@pytest.mark.asyncio
async def test_full_clarify_roundtrip():
    from echo_agent.agent.pipeline.inference_stage import InferenceStage

    mgr = ClarifyManager()
    cog = _FakeCog()
    stage = InferenceStage.__new__(InferenceStage)
    stage._clarify = mgr
    stage._cog = cog

    tool = ClarifyTool(manager=mgr)
    tc = _FakeToolCall("clarify", {"question": "用哪个方案?", "options": ["A", "B"]})

    # 1) pipeline pre-registers, injects id, emits the frame
    await stage._prepare_clarify(tc, _FakeEvent())
    cid = tc.arguments["_clarify_id"]
    assert cog.emitted[0][0] == "clarify_request"
    assert cog.emitted[0][1]["clarify_id"] == cid

    # 2) the tool starts blocking (as the pipeline would await it)
    ctx = ToolExecutionContext(channel="gateway:cli", user_id="u1")
    exec_task = asyncio.create_task(tool.execute(tc.arguments, ctx))

    # 3) user replies /clarify <id> B → resolve
    await asyncio.sleep(0.01)
    assert mgr.resolve(cid, "B") is True

    # 4) the tool unblocks and returns the user's answer to the model
    result = await asyncio.wait_for(exec_task, timeout=1.0)
    assert result.success is True
    assert result.output == "B"
