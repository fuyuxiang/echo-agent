import inspect

import pytest

from echo_agent.agent.pipeline.context_stage import ContextStage
from echo_agent.agent.pipeline.inference_stage import InferenceStage
from echo_agent.bus.events import InboundEvent


def test_stages_accept_cognitive_emitter():
    assert "cognitive_emitter" in inspect.signature(InferenceStage.__init__).parameters
    assert "cognitive_emitter" in inspect.signature(ContextStage.__init__).parameters


class _CapEmitter:
    def __init__(self):
        self.calls = []

    async def emit(self, event, cog_type, data, summary):
        self.calls.append((cog_type, data, summary))


class _FakeEntry:
    """Mimics a real memory entry object (the actual (entry, score) call-site shape)."""

    def __init__(self, key, content, entry_id, source):
        self.key = key
        self.content = content
        self.id = entry_id
        self.source = source


@pytest.mark.asyncio
async def test_context_stage_emits_memory_recalled():
    # Build a minimal ContextStage and drive its recall-emission helper directly.
    cap = _CapEmitter()
    stage = ContextStage.__new__(ContextStage)
    stage._cog = cap
    ev = InboundEvent.text_message(
        channel="gateway:cli", sender_id="u", chat_id="c", text="hi"
    )
    # Dict form (brief contract): normalized via content/source/score keys.
    scored = [
        {"content": "喜欢深色主题", "source": "user_stated", "score": 0.91},
        {"content": "在上海", "source": "consolidated", "score": 0.7},
    ]
    await stage._emit_memory_recalled(ev, scored)
    assert cap.calls[0][0] == "memory_recalled"
    assert cap.calls[0][1]["items"][0]["source"] == "user_stated"
    assert cap.calls[0][1]["items"][0]["score"] == 0.91
    assert "2" in cap.calls[0][2]  # summary carries the count


@pytest.mark.asyncio
async def test_context_stage_emits_memory_recalled_tuple_form():
    # Real call-site form: (entry, score) tuples from filtered mem_items.
    cap = _CapEmitter()
    stage = ContextStage.__new__(ContextStage)
    stage._cog = cap
    ev = InboundEvent.text_message(
        channel="gateway:cli", sender_id="u", chat_id="c", text="hi"
    )
    scored = [
        (_FakeEntry("pref", "喜欢深色主题", "id1", "user_stated"), 0.88),
        (_FakeEntry("loc", "在上海", "id2", "legacy"), 0.5),
    ]
    await stage._emit_memory_recalled(ev, scored)
    item = cap.calls[0][1]["items"][0]
    assert cap.calls[0][0] == "memory_recalled"
    assert item["content"] == "喜欢深色主题"
    assert item["source"] == "user_stated"
    assert item["score"] == 0.88
    assert "2" in cap.calls[0][2]


@pytest.mark.asyncio
async def test_context_stage_recall_noop_without_emitter():
    stage = ContextStage.__new__(ContextStage)
    stage._cog = None
    ev = InboundEvent.text_message(
        channel="gateway:cli", sender_id="u", chat_id="c", text="hi"
    )
    # No emitter and empty list must both be safe no-ops.
    await stage._emit_memory_recalled(ev, [{"content": "x"}])
    stage._cog = _CapEmitter()
    await stage._emit_memory_recalled(ev, [])
    assert stage._cog.calls == []


@pytest.mark.asyncio
async def test_inference_stage_emits_tool_call_and_cost():
    cap = _CapEmitter()
    stage = InferenceStage.__new__(InferenceStage)
    stage._cog = cap
    ev = InboundEvent.text_message(
        channel="gateway:cli", sender_id="u", chat_id="c", text="hi"
    )
    await stage._emit_tool_call(ev, "edit", {"path": "a.py"}, "ok", "已写入 3 行")
    await stage._emit_cost(ev, 1200, 0.012, 0.033)
    assert cap.calls[0][0] == "tool_call"
    assert cap.calls[0][1]["name"] == "edit"
    assert cap.calls[0][1]["status"] == "ok"
    assert cap.calls[0][1]["params"]["path"] == "a.py"
    assert cap.calls[1][0] == "cost_update"
    assert cap.calls[1][1]["total_cost"] == 0.033
    assert cap.calls[1][1]["turn_tokens"] == 1200


@pytest.mark.asyncio
async def test_inference_stage_cost_and_tool_noop_without_emitter():
    stage = InferenceStage.__new__(InferenceStage)
    stage._cog = None
    ev = InboundEvent.text_message(
        channel="gateway:cli", sender_id="u", chat_id="c", text="hi"
    )
    await stage._emit_tool_call(ev, "edit", {"path": "a.py"}, "ok", "x")
    await stage._emit_cost(ev, 1200, 0.012, 0.033)  # must be safe no-ops
