"""Tests for AgentLoop core processing logic."""

from __future__ import annotations

import asyncio
from pathlib import Path
from unittest.mock import AsyncMock, MagicMock, patch

import pytest

from echo_agent.bus.events import InboundEvent, OutboundEvent
from echo_agent.bus.queue import MessageBus
from echo_agent.models.provider import LLMProvider, LLMResponse, ToolCallRequest
from echo_agent.session.manager import SessionManager


class _StubProvider(LLMProvider):
    def __init__(self, responses: list[LLMResponse] | None = None):
        super().__init__()
        self._responses = responses or [LLMResponse(content="hello", finish_reason="stop")]
        self._call_idx = 0

    async def chat(self, messages, tools=None, model=None, tool_choice=None, **kwargs):
        idx = min(self._call_idx, len(self._responses) - 1)
        self._call_idx += 1
        return self._responses[idx]

    async def chat_stream(self, messages, tools=None, model=None, tool_choice=None, on_delta=None, **kwargs):
        resp = await self.chat(messages, tools, model, tool_choice, **kwargs)
        if resp.content and on_delta and resp.finish_reason != "error":
            result = on_delta(resp.content)
            if asyncio.iscoroutine(result):
                await result
        return resp

    def get_default_model(self):
        return "stub"


def _make_agent_loop(tmp_path: Path, provider: _StubProvider | None = None):
    from echo_agent.agent.loop import AgentLoop
    from echo_agent.config.loader import load_config

    config = load_config(overrides={"workspace": str(tmp_path)})
    bus = MessageBus()
    prov = provider or _StubProvider()

    loop = AgentLoop(
        bus=bus,
        config=config,
        provider=prov,
        workspace=tmp_path,
    )
    return loop, bus, prov


@pytest.mark.asyncio
async def test_process_event_simple_response(tmp_path: Path) -> None:
    agent, bus, _ = _make_agent_loop(tmp_path)
    event = InboundEvent.text_message(channel="test", sender_id="u1", chat_id="c1", text="hi")

    result = await agent._process_event(event, "trace1")

    assert "hello" in result.response_text


@pytest.mark.asyncio
async def test_process_event_llm_error_breaks_loop(tmp_path: Path) -> None:
    provider = _StubProvider([
        LLMResponse(content="Error: 500 server error", finish_reason="error"),
    ])
    agent, bus, _ = _make_agent_loop(tmp_path, provider)
    event = InboundEvent.text_message(channel="test", sender_id="u1", chat_id="c1", text="hi")

    result = await agent._process_event(event, "trace2")

    assert "issue" in result.response_text.lower() or "error" in result.response_text.lower()


@pytest.mark.asyncio
async def test_process_event_circuit_breaker_consecutive_failures(tmp_path: Path) -> None:
    tool_call = ToolCallRequest(id="tc1", name="nonexistent_tool", arguments={})
    provider = _StubProvider([
        LLMResponse(content="", finish_reason="tool_calls", tool_calls=[tool_call]),
        LLMResponse(content="", finish_reason="tool_calls", tool_calls=[tool_call]),
        LLMResponse(content="", finish_reason="tool_calls", tool_calls=[tool_call]),
        LLMResponse(content="gave up", finish_reason="stop"),
    ])
    agent, bus, _ = _make_agent_loop(tmp_path, provider)
    event = InboundEvent.text_message(channel="test", sender_id="u1", chat_id="c1", text="do something")

    result = await agent._process_event(event, "trace3")

    # After 3 consecutive failures, tools should be disabled and model forced to respond
    assert "gave up" in result.response_text


@pytest.mark.asyncio
async def test_on_inbound_session_lock_serializes(tmp_path: Path) -> None:
    agent, bus, _ = _make_agent_loop(tmp_path)
    agent._running = True
    order: list[str] = []

    original_process = agent._process_event

    async def tracked_process(event, trace_id, **kwargs):
        order.append(f"start:{event.text}")
        await asyncio.sleep(0.01)
        result = await original_process(event, trace_id, **kwargs)
        order.append(f"end:{event.text}")
        return result

    agent._process_event = tracked_process

    e1 = InboundEvent.text_message(channel="test", sender_id="u1", chat_id="c1", text="msg1")
    e2 = InboundEvent.text_message(channel="test", sender_id="u1", chat_id="c1", text="msg2")
    # Same session_key → serialized
    e1._session_key_override = "test:c1"
    e2._session_key_override = "test:c1"

    await asyncio.gather(agent._on_inbound(e1), agent._on_inbound(e2))

    # Should be serialized: start1, end1, start2, end2 (or 2 before 1)
    starts = [x for x in order if x.startswith("start:")]
    ends = [x for x in order if x.startswith("end:")]
    # First start's end should come before second start
    first_end_idx = order.index(ends[0])
    second_start_idx = order.index(starts[1])
    assert first_end_idx < second_start_idx


@pytest.mark.asyncio
async def test_on_inbound_error_sends_error_reply(tmp_path: Path) -> None:
    agent, bus, _ = _make_agent_loop(tmp_path)
    agent._running = True
    published: list[OutboundEvent] = []

    async def capture_outbound(event: OutboundEvent) -> None:
        published.append(event)

    bus.subscribe_outbound_global(capture_outbound)

    agent._process_event = AsyncMock(side_effect=RuntimeError("test crash"))

    event = InboundEvent.text_message(channel="test", sender_id="u1", chat_id="c1", text="crash me")
    await agent._on_inbound(event)

    assert len(published) >= 1
    error_msg = published[-1].content[0].text
    assert "error" in error_msg.lower()


@pytest.mark.asyncio
async def test_approval_command_saves_session(tmp_path: Path) -> None:
    agent, bus, _ = _make_agent_loop(tmp_path)
    event = InboundEvent.text_message(channel="test", sender_id="u1", chat_id="c1", text="/approvals")

    result = await agent._process_event(event, "trace_approval")

    # Should return a response (even if no pending approvals)
    assert result.response_text is not None

    # Session should have been saved with the user message
    session = await agent.sessions.get_or_create(event.session_key)
    user_msgs = [m for m in session.messages if m.get("role") == "user"]
    assert any("/approvals" in m.get("content", "") for m in user_msgs)
