import pytest
from echo_agent.agent.cognitive_emitter import CognitiveEmitter, should_emit_cognitive
from echo_agent.bus.events import InboundEvent


class FakeBus:
    def __init__(self):
        self.published = []
    async def publish_outbound(self, out):
        self.published.append(out)


def _inbound(channel):
    return InboundEvent.text_message(
        channel=channel, sender_id="u1", chat_id="c1", text="hi"
    )


def test_gate_only_cli():
    assert should_emit_cognitive("gateway:cli") is True
    assert should_emit_cognitive("gateway:wechat") is False
    assert should_emit_cognitive("cli") is False
    assert should_emit_cognitive("") is False


@pytest.mark.asyncio
async def test_emit_publishes_cognitive_frame_for_cli():
    bus = FakeBus()
    emitter = CognitiveEmitter(bus)
    ev = _inbound("gateway:cli")
    await emitter.emit(ev, "memory_recalled", {"items": [1, 2, 3]}, "召回 3 条记忆")
    assert len(bus.published) == 1
    out = bus.published[0]
    assert out.message_kind == "cognitive"
    assert out.is_final is False
    assert out.text == "召回 3 条记忆"
    assert out.metadata["cog_type"] == "memory_recalled"
    assert out.metadata["_inbound_event_id"] == ev.event_id
    assert out.metadata["cog_event_id"].startswith("evt_")
    assert out.metadata["data"] == {"items": [1, 2, 3]}


@pytest.mark.asyncio
async def test_emit_skips_non_cli():
    bus = FakeBus()
    emitter = CognitiveEmitter(bus)
    await emitter.emit(_inbound("gateway:wechat"), "tool_call", {"name": "x"}, "工具")
    assert bus.published == []
