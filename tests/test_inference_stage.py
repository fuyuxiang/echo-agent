"""Tests for InferenceStage — LLM call loop with tool execution and circuit breaking."""

from __future__ import annotations

import hashlib
from unittest.mock import AsyncMock, MagicMock, patch

import pytest

from echo_agent.agent.pipeline.inference_stage import InferenceStage
from echo_agent.agent.pipeline.types import InferenceResult, PipelineContext
from echo_agent.models.provider import LLMResponse, ToolCallRequest
from echo_agent.models.router import RouteDecision


def _make_config():
    config = MagicMock()
    config.skills = MagicMock()
    config.skills.creation_nudge_interval = 0
    config.memory = MagicMock()
    config.memory.memory_nudge_interval = 0
    return config


def _make_event():
    event = MagicMock()
    event.channel = "test"
    event.chat_id = "chat_1"
    event.reply_to_id = None
    event.event_id = "evt_001"
    event.metadata = {}
    event.session_key = "test:chat_1"
    event.sender_id = "user_1"
    event.text = "hello"
    return event


def _make_session():
    session = MagicMock()
    session.add_message = MagicMock()
    return session


def _make_ctx(event=None, session=None, messages=None, tool_defs=None, publish_response=True):
    ctx = PipelineContext(
        event=event or _make_event(),
        session=session or _make_session(),
        trace_id="trace_001",
        publish_response=publish_response,
    )
    ctx.messages = messages or [{"role": "user", "content": "hello"}]
    ctx.tool_defs = tool_defs or []
    ctx.stream_publisher = MagicMock()
    ctx.stream_publisher.on_delta = AsyncMock()
    ctx.execution_plan = None
    ctx.task_type = "chat"
    return ctx


def _make_stage(provider=None, tools=None, approval_gate=None, max_iterations=10):
    config = _make_config()
    bus = AsyncMock()
    bus.publish_outbound = AsyncMock()
    prov = provider or AsyncMock()
    if not provider:
        prov.chat_stream_with_retry = AsyncMock(
            return_value=LLMResponse(content="done", finish_reason="stop")
        )

    tools_reg = tools or MagicMock()
    if not tools:
        tools_reg.execute = AsyncMock()
        tools_reg.has = MagicMock(return_value=False)

    gate = approval_gate or AsyncMock()
    if not approval_gate:
        gate.check = AsyncMock(return_value=MagicMock(denial=None, approved_actions=frozenset()))

    tracer = MagicMock()
    tracer.start_span = MagicMock(return_value="span_1")
    tracer.end_span = MagicMock()

    telemetry = MagicMock()
    telemetry.available = False

    inference_ctrl = MagicMock()
    inference_ctrl.validate_response = MagicMock(return_value=[])

    circuit_breaker = MagicMock()
    circuit_breaker.get_unavailable_tools = MagicMock(return_value=set())
    circuit_breaker.record_success = MagicMock()
    circuit_breaker.record_failure = MagicMock()

    credentials = MagicMock()
    credentials.get_for_tool = MagicMock(return_value={})

    stage = InferenceStage(
        config=config,
        bus=bus,
        provider=prov,
        router=None,
        tools=tools_reg,
        approval_gate=gate,
        credentials=credentials,
        tracer=tracer,
        telemetry=telemetry,
        inference=inference_ctrl,
        circuit_breaker=circuit_breaker,
        default_model="test-model",
        max_iterations=max_iterations,
    )
    return stage, bus


class TestInferenceStageTextOnly:
    """Model returns plain text with no tool calls — immediate completion."""

    @pytest.mark.asyncio
    async def test_plain_text_response(self):
        provider = AsyncMock()
        provider.chat_stream_with_retry = AsyncMock(
            return_value=LLMResponse(content="Hello world", finish_reason="stop")
        )
        stage, bus = _make_stage(provider=provider)
        ctx = _make_ctx()

        result = await stage.run(ctx)

        assert result.response_text == "Hello world"
        assert result.total_tool_calls == 0


class TestInferenceStageError:
    """Model returns finish_reason='error' — fallback text."""

    @pytest.mark.asyncio
    async def test_error_response_fallback(self):
        provider = AsyncMock()
        provider.chat_stream_with_retry = AsyncMock(
            return_value=LLMResponse(content="LLM error occurred", finish_reason="error")
        )
        stage, bus = _make_stage(provider=provider)
        ctx = _make_ctx()

        result = await stage.run(ctx)

        assert "issue" in result.response_text.lower() or "try again" in result.response_text.lower()


class TestInferenceStageApprovalDenied:
    """Tool call rejected by ApprovalGate — returns rejection message."""

    @pytest.mark.asyncio
    async def test_tool_denied(self):
        from echo_agent.agent.tools.base import ToolResult

        provider = AsyncMock()
        tc = ToolCallRequest(id="call_1", name="exec", arguments={"command": "rm -rf /"})
        provider.chat_stream_with_retry = AsyncMock(side_effect=[
            LLMResponse(content="Let me run that", tool_calls=[tc], finish_reason="tool_calls"),
            LLMResponse(content="Understood, denied.", finish_reason="stop"),
        ])

        denial = ToolResult(success=False, error="Permission denied: dangerous command")
        gate = AsyncMock()
        gate.check = AsyncMock(return_value=MagicMock(denial=denial, approved_actions=frozenset()))

        stage, bus = _make_stage(provider=provider, approval_gate=gate)
        ctx = _make_ctx()

        result = await stage.run(ctx)

        assert result.total_tool_calls == 1
        # The second LLM call responds after seeing the denial
        assert result.response_text == "Understood, denied."


class TestInferenceStageRepeatGuard:
    """Same tool + arguments called >= 4 times is blocked."""

    @pytest.mark.asyncio
    async def test_repeat_call_blocked(self):
        tc = ToolCallRequest(id="call_1", name="search", arguments={"q": "test"})

        provider = AsyncMock()
        # Return tool calls 5 times, then stop
        provider.chat_stream_with_retry = AsyncMock(side_effect=[
            LLMResponse(content="", tool_calls=[tc], finish_reason="tool_calls"),
            LLMResponse(content="", tool_calls=[tc], finish_reason="tool_calls"),
            LLMResponse(content="", tool_calls=[tc], finish_reason="tool_calls"),
            LLMResponse(content="", tool_calls=[tc], finish_reason="tool_calls"),
            LLMResponse(content="Done after block", finish_reason="stop"),
        ])

        tools_reg = MagicMock()
        tool_result = MagicMock(success=True, text="result", error=None, metadata={})
        tools_reg.execute = AsyncMock(return_value=tool_result)
        tools_reg.has = MagicMock(return_value=False)

        stage, bus = _make_stage(provider=provider, tools=tools_reg)
        ctx = _make_ctx()

        result = await stage.run(ctx)

        # The 4th call should be blocked (threshold is 4)
        assert result.response_text == "Done after block"


class TestInferenceStageMaxIterations:
    """Loop exhausts max_iterations — fallback text."""

    @pytest.mark.asyncio
    async def test_max_iterations_fallback(self):
        tc = ToolCallRequest(id="call_1", name="tool_a", arguments={"x": "1"})
        tc2 = ToolCallRequest(id="call_2", name="tool_b", arguments={"y": "2"})

        provider = AsyncMock()
        # Always return tool calls, never stop
        provider.chat_stream_with_retry = AsyncMock(
            return_value=LLMResponse(content="", tool_calls=[tc], finish_reason="tool_calls")
        )

        tools_reg = MagicMock()
        tool_result = MagicMock(success=True, text="ok", error=None, metadata={})
        tools_reg.execute = AsyncMock(return_value=tool_result)
        tools_reg.has = MagicMock(return_value=False)

        stage, bus = _make_stage(provider=provider, tools=tools_reg, max_iterations=3)
        ctx = _make_ctx()

        result = await stage.run(ctx)

        # Loop exhausted, fallback text should mention issue/try again
        assert "issue" in result.response_text.lower() or "try again" in result.response_text.lower()
