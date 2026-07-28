"""Streamed thinking on the inference stage: the sink that publishes snapshots
mid-round, and the settle step that closes, retracts or replaces them."""

from __future__ import annotations

import pytest

from echo_agent.agent.pipeline.inference_stage import InferenceStage
from echo_agent.agent.thinking_stream import ThinkingStream
from echo_agent.bus.events import InboundEvent
from echo_agent.models.provider import LLMResponse


class _CapEmitter:
    def __init__(self):
        self.calls = []

    async def emit(self, event, cog_type, data, summary):
        self.calls.append((cog_type, data, summary))


def _stage(emitter=None) -> InferenceStage:
    stage = InferenceStage.__new__(InferenceStage)
    stage._cog = emitter
    return stage


def _event(channel="gateway:cli") -> InboundEvent:
    return InboundEvent.text_message(
        channel=channel, sender_id="u", chat_id="c", text="hi"
    )


def _thinking_frames(cap):
    return [c for c in cap.calls if c[0] == "thinking"]


@pytest.mark.asyncio
async def test_sink_publishes_a_streaming_snapshot():
    cap = _CapEmitter()
    stage = _stage(cap)
    stream = ThinkingStream(flush_chars=1, flush_interval=0.0)
    sink = stage._thinking_sink(_event(), stream, 0.0)
    await sink("先看看调用链")
    (_, data, summary) = _thinking_frames(cap)[0]
    assert data["streaming"] is True
    assert data["text"] == "先看看调用链"
    assert data["thinking_id"] == stream.thinking_id
    # Mid-stream there is no duration to report yet, so the summary must not
    # claim one — it would then count backwards on the final frame.
    assert summary == "思考中"


@pytest.mark.asyncio
async def test_sink_holds_deltas_that_the_pacing_rejects():
    cap = _CapEmitter()
    stage = _stage(cap)
    stream = ThinkingStream(flush_chars=1000, flush_interval=1000.0)
    sink = stage._thinking_sink(_event(), stream, 0.0)
    await sink("a")   # opening frame always goes out
    await sink("b")   # under both budgets: held
    assert len(_thinking_frames(cap)) == 1


@pytest.mark.asyncio
async def test_no_sink_for_channels_that_render_no_cognitive_frames():
    stage = _stage(_CapEmitter())
    # None rather than a no-op callback, so the provider skips its forwarding
    # branch instead of building deltas nobody reads.
    assert stage._thinking_sink(_event("im:wecom"), ThinkingStream(), 0.0) is None


@pytest.mark.asyncio
async def test_no_sink_without_an_emitter():
    stage = _stage(None)
    assert stage._thinking_sink(_event(), ThinkingStream(), 0.0) is None


@pytest.mark.asyncio
async def test_settle_closes_a_streamed_trace_with_its_duration():
    cap = _CapEmitter()
    stage = _stage(cap)
    stream = ThinkingStream(flush_chars=1, flush_interval=0.0)
    sink = stage._thinking_sink(_event(), stream, 0.0)
    await sink("想了一下")
    resp = LLMResponse(content="答案", reasoning_content="想了一下，再想了一下")
    await stage._settle_thinking(_event(), stream, resp, 3200)

    frames = _thinking_frames(cap)
    assert len(frames) == 2
    _, data, summary = frames[-1]
    assert data["streaming"] is False
    assert data["retracted"] is False
    # Same id as the partials, so the client updates one line rather than
    # leaving the partial behind and mounting a second block.
    assert data["thinking_id"] == stream.thinking_id
    assert data["text"] == "想了一下，再想了一下"
    assert summary == "思考 3.2s"


@pytest.mark.asyncio
async def test_settle_retracts_when_the_reasoning_became_the_answer():
    cap = _CapEmitter()
    stage = _stage(cap)
    stream = ThinkingStream(flush_chars=1, flush_interval=0.0)
    sink = stage._thinking_sink(_event(), stream, 0.0)
    await sink("答案正文")
    # _promote_reasoning moved the text into content and cleared the slot; the
    # reply body is about to show it, so the trace must not show it too.
    resp = LLMResponse(content="答案正文", reasoning_content=None)
    await stage._settle_thinking(_event(), stream, resp, 1000)

    _, data, _ = _thinking_frames(cap)[-1]
    assert data["retracted"] is True
    assert data["thinking_id"] == stream.thinking_id


@pytest.mark.asyncio
async def test_settle_emits_once_for_a_provider_that_never_streams():
    cap = _CapEmitter()
    stage = _stage(cap)
    stream = ThinkingStream()
    resp = LLMResponse(content="答案", reasoning_content="完整推理")
    await stage._settle_thinking(_event(), stream, resp, 900)

    frames = _thinking_frames(cap)
    assert len(frames) == 1
    _, data, _ = frames[0]
    assert data["streaming"] is False
    assert data["retracted"] is False
    assert data["text"] == "完整推理"


@pytest.mark.asyncio
async def test_settle_stays_silent_when_there_was_no_reasoning_at_all():
    cap = _CapEmitter()
    stage = _stage(cap)
    resp = LLMResponse(content="答案", reasoning_content=None)
    await stage._settle_thinking(_event(), ThinkingStream(), resp, 900)
    assert _thinking_frames(cap) == []
