"""Tests for AgentLoop core processing logic."""

from __future__ import annotations

import asyncio
from pathlib import Path
from unittest.mock import AsyncMock

import pytest

from echo_agent.bus.events import InboundEvent, OutboundEvent
from echo_agent.bus.queue import MessageBus
from echo_agent.models.provider import LLMProvider, LLMResponse, ToolCallRequest


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
async def test_process_direct_holds_session_lock(tmp_path: Path) -> None:
    """process_direct used to bypass the session lock that the inbound
    dispatcher uses, so two concurrent CLI calls on the same session_key
    could interleave their writes to the message history. The lock now
    serialises them just like normal inbound traffic."""
    agent, bus, _ = _make_agent_loop(tmp_path)
    order: list[str] = []

    original = agent._process_event

    async def tracked(event, trace_id, **kwargs):
        order.append(f"start:{event.text}")
        await asyncio.sleep(0.05)
        result = await original(event, trace_id, **kwargs)
        order.append(f"end:{event.text}")
        return result

    agent._process_event = tracked

    await asyncio.gather(
        agent.process_direct("hi-1", session_key="cli:same"),
        agent.process_direct("hi-2", session_key="cli:same"),
    )

    # Lock must serialize: each call's start/end must wrap before the other
    # starts. No "start:hi-1, start:hi-2, end:hi-1, end:hi-2" interleaving.
    starts = [s for s in order if s.startswith("start:")]
    ends = [s for s in order if s.startswith("end:")]
    assert len(starts) == 2 and len(ends) == 2
    # The first end must come before the second start.
    assert order.index(ends[0]) < order.index(starts[1])


@pytest.mark.asyncio
async def test_tool_cancellation_records_circuit_breaker_failure(tmp_path: Path) -> None:
    """When a tool is interrupted (CancelledError), the inference loop's
    BaseException handler must still call circuit_breaker.record_failure
    so the breaker eventually trips on a stuck tool. Without this, an
    interrupted tool just stays "untracked" and the breaker stat skews."""
    from echo_agent.agent.tools.base import Tool, ToolResult

    class _SlowTool(Tool):
        name = "slow_tool"
        description = "stub that hangs forever"
        parameters = {"type": "object", "properties": {}, "required": []}
        timeout_seconds = 60

        async def execute(self, params, ctx=None):
            await asyncio.sleep(99)
            return ToolResult(success=True, output="never")

    provider = _StubProvider([
        LLMResponse(
            content="",
            finish_reason="tool_calls",
            tool_calls=[ToolCallRequest(id="tc1", name="slow_tool", arguments={})],
        ),
    ])
    agent, bus, _ = _make_agent_loop(tmp_path, provider)
    agent.tools.register(_SlowTool())
    event = InboundEvent.text_message(channel="test", sender_id="u1", chat_id="c1", text="hang")

    task = asyncio.create_task(agent._process_event(event, "trace_cancel"))
    await asyncio.sleep(0.05)
    task.cancel()
    try:
        await task
    except (asyncio.CancelledError, BaseException):
        pass

    # The breaker should have recorded the failure even though the tool
    # never returned a normal result.
    assert agent._circuit_breaker._circuits.get("slow_tool") is not None
    assert agent._circuit_breaker._circuits["slow_tool"].failure_count >= 1


@pytest.mark.asyncio
async def test_repeated_identical_tool_call_is_short_circuited(tmp_path: Path) -> None:
    """The 4th identical tool call must be blocked WITHOUT actually invoking
    the tool. Old code only renamed the displayed message and let the tool
    keep running. We assert that after the threshold the underlying tool
    isn't invoked again.
    """
    from echo_agent.agent.tools.base import Tool, ToolResult

    invocations: list[dict] = []

    class _SideEffectTool(Tool):
        name = "se_tool"
        description = "side-effect tool used to verify repeat blocking"
        parameters = {"type": "object", "properties": {}, "required": []}

        async def execute(self, params, ctx=None):
            invocations.append(dict(params))
            return ToolResult(success=True, output="ok")

    # Five identical tool calls then a final stop. Block kicks in on the 4th.
    responses = [
        LLMResponse(
            content="",
            finish_reason="tool_calls",
            tool_calls=[ToolCallRequest(id=f"tc{i}", name="se_tool", arguments={"x": 1})],
        )
        for i in range(5)
    ] + [LLMResponse(content="done", finish_reason="stop")]
    provider = _StubProvider(responses)
    agent, bus, _ = _make_agent_loop(tmp_path, provider)
    agent.tools.register(_SideEffectTool())
    event = InboundEvent.text_message(channel="test", sender_id="u1", chat_id="c1", text="repeat")

    await agent._process_event(event, "trace_repeat")

    # Real invocations are bounded — the repeat tracker cuts off well before
    # all five identical calls reach the tool. Without the fix the 4th and
    # 5th would still execute (only the displayed text was being rewritten).
    assert len(invocations) <= 3, (
        f"expected at most 3 real executions, got {len(invocations)}: {invocations}"
    )
    # And at least the very first call DID run — proving we're not over-blocking.
    assert len(invocations) >= 1


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
    # Hard-exception path now delivers the Chinese generic fallback (no raw
    # exception string leaked to the user); see loop._on_inbound except block.
    from echo_agent.agent.degraded_notice import GENERIC_FALLBACK_TEXT
    assert error_msg == GENERIC_FALLBACK_TEXT


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


# ── 审批决策门表征：approve/deny 两条出口 ────────────────────────────────────


@pytest.mark.asyncio
async def test_approve_command_resolves_pending_request(tmp_path: Path) -> None:
    """/approve <id>：命中待审批请求 → 通过 ApprovalManager 批准，
    请求移出 pending，决策为 APPROVED。"""
    from echo_agent.permissions.manager import ApprovalStatus

    agent, _, _ = _make_agent_loop(tmp_path)
    # 手动落一个 pending 请求，表征「人工批准」分支。
    req = agent.approval.request_approval(action="shell", tool_name="bash", user_id="")
    agent.approval._pending[req.id] = req

    event = InboundEvent.text_message(
        channel="test", sender_id="u1", chat_id="c1", text=f"/approve {req.id}"
    )
    resp = await agent._handle_approval_command(event)

    assert resp == f"Approval request {req.id} approved."
    assert agent.approval.get(req.id) is None  # 已移出 pending
    decided = agent.approval._find_history(req.id)
    assert decided is not None and decided.status == ApprovalStatus.APPROVED


@pytest.mark.asyncio
async def test_deny_command_resolves_pending_request(tmp_path: Path) -> None:
    """/deny <id> <reason>：命中待审批请求 → 拒绝，记录 reason，
    请求移出 pending，决策为 DENIED。"""
    from echo_agent.permissions.manager import ApprovalStatus

    agent, _, _ = _make_agent_loop(tmp_path)
    req = agent.approval.request_approval(action="shell", tool_name="bash", user_id="")
    agent.approval._pending[req.id] = req

    event = InboundEvent.text_message(
        channel="test", sender_id="u1", chat_id="c1", text=f"/deny {req.id} too risky"
    )
    resp = await agent._handle_approval_command(event)

    assert resp == f"Approval request {req.id} denied."
    assert agent.approval.get(req.id) is None
    decided = agent.approval._find_history(req.id)
    assert decided is not None and decided.status == ApprovalStatus.DENIED
    assert decided.reason == "too risky"


@pytest.mark.asyncio
async def test_approve_unknown_request_returns_not_found(tmp_path: Path) -> None:
    """/approve 未知 id：不命中任何 pending → 返回 not found。"""
    agent, _, _ = _make_agent_loop(tmp_path)
    event = InboundEvent.text_message(
        channel="test", sender_id="u1", chat_id="c1", text="/approve nope123"
    )
    resp = await agent._handle_approval_command(event)
    assert resp == "Approval request not found: nope123"


@pytest.mark.asyncio
async def test_non_approval_command_passes_through(tmp_path: Path) -> None:
    """非审批命令文本 → _handle_approval_command 返回 None（不拦截）。"""
    agent, _, _ = _make_agent_loop(tmp_path)
    event = InboundEvent.text_message(
        channel="test", sender_id="u1", chat_id="c1", text="hello there"
    )
    resp = await agent._handle_approval_command(event)
    assert resp is None
