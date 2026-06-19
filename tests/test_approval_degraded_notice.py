from __future__ import annotations

import pytest

from echo_agent.agent.degraded_notice import notice_for, REASON_APPROVAL_UNAVAILABLE
from echo_agent.agent.streaming import ProcessResult
from echo_agent.bus.events import InboundEvent, ContentBlock, ContentType


def _make_loop():
    """Build an AgentLoop with _process_event stubbed, capturing outbound."""
    from echo_agent.agent.loop import AgentLoop
    loop = AgentLoop.__new__(AgentLoop)  # bypass heavy __init__
    sent: list = []

    class _Bus:
        async def publish_outbound(self, out):
            sent.append(out)

    class _Sessions:
        async def acquire(self, key):
            import asyncio
            return asyncio.Lock()

    class _Tracer:
        def start_span(self, *a, **k): return None
        def end_span(self, *a, **k): pass
        def flush_trace(self, *a, **k): pass

    loop.bus = _Bus()
    loop.sessions = _Sessions()
    loop.tracer = _Tracer()
    loop._running = True
    loop.config = None
    return loop, sent


def _event():
    return InboundEvent(
        channel="weixin", sender_id="u1", chat_id="c1",
        content=[ContentBlock(type=ContentType.TEXT, text="research")],
    )


@pytest.mark.asyncio
async def test_empty_response_with_notice_sends_chinese(monkeypatch):
    loop, sent = _make_loop()
    notice = notice_for(REASON_APPROVAL_UNAVAILABLE)

    async def fake_process(event, trace_id, publish_response=False):
        return ProcessResult(response_text="", outbound_sent=False, degraded_notices=[notice])

    monkeypatch.setattr(loop, "_process_event", fake_process)
    monkeypatch.setattr(loop, "_is_approval_command", lambda t: False)
    await loop._on_inbound(_event())
    assert len(sent) == 1
    assert "安全审批暂时不可用" in sent[0].text
    assert sent[0].is_final is True


@pytest.mark.asyncio
async def test_empty_response_no_notice_sends_generic_chinese(monkeypatch):
    loop, sent = _make_loop()

    async def fake_process(event, trace_id, publish_response=False):
        return ProcessResult(response_text="", outbound_sent=False, degraded_notices=[])

    monkeypatch.setattr(loop, "_process_event", fake_process)
    monkeypatch.setattr(loop, "_is_approval_command", lambda t: False)
    await loop._on_inbound(_event())
    assert len(sent) == 1
    assert sent[0].text.startswith("⚠️")


@pytest.mark.asyncio
async def test_generic_english_replaced_by_notice(monkeypatch):
    loop, sent = _make_loop()
    notice = notice_for(REASON_APPROVAL_UNAVAILABLE)
    english = "I encountered an issue processing your request. Please try again or rephrase your question."

    async def fake_process(event, trace_id, publish_response=False):
        return ProcessResult(response_text=english, outbound_sent=False, degraded_notices=[notice])

    monkeypatch.setattr(loop, "_process_event", fake_process)
    monkeypatch.setattr(loop, "_is_approval_command", lambda t: False)
    await loop._on_inbound(_event())
    assert len(sent) == 1
    assert english not in sent[0].text
    assert "安全审批暂时不可用" in sent[0].text


@pytest.mark.asyncio
async def test_real_answer_already_sent_appends_notice(monkeypatch):
    loop, sent = _make_loop()
    notice = notice_for(REASON_APPROVAL_UNAVAILABLE)

    async def fake_process(event, trace_id, publish_response=False):
        return ProcessResult(response_text="真实回答", outbound_sent=True, degraded_notices=[notice])

    monkeypatch.setattr(loop, "_process_event", fake_process)
    monkeypatch.setattr(loop, "_is_approval_command", lambda t: False)
    await loop._on_inbound(_event())
    # main answer already streamed; notice delivered as a single follow-up
    assert len(sent) == 1
    assert "安全审批暂时不可用" in sent[0].text


@pytest.mark.asyncio
async def test_real_answer_no_notice_unchanged(monkeypatch):
    loop, sent = _make_loop()

    async def fake_process(event, trace_id, publish_response=False):
        return ProcessResult(response_text="真实回答", outbound_sent=False, degraded_notices=[])

    monkeypatch.setattr(loop, "_process_event", fake_process)
    monkeypatch.setattr(loop, "_is_approval_command", lambda t: False)
    await loop._on_inbound(_event())
    assert len(sent) == 1
    assert sent[0].text == "真实回答"
