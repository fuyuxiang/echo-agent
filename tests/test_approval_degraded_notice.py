from __future__ import annotations

from unittest.mock import AsyncMock, MagicMock

import pytest

from echo_agent.agent.approval_gate import ApprovalGate
from echo_agent.agent.degraded_notice import notice_for, REASON_APPROVAL_UNAVAILABLE
from echo_agent.agent.streaming import ProcessResult
from echo_agent.bus.events import InboundEvent, ContentBlock, ContentType
from echo_agent.bus.queue import MessageBus
from echo_agent.config.loader import load_config
from echo_agent.permissions.manager import ApprovalManager


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
    # _on_inbound 解析群聊会话作用域时读 config.session.group_session_scope；
    # 接线心跳后还会读 config.agent.heartbeat。用真实 SessionConfig/HeartbeatConfig
    # 提供默认值；心跳置 enabled=False，使这些用例只聚焦降级通知行为。
    from types import SimpleNamespace
    from echo_agent.config.schema import HeartbeatConfig, SessionConfig
    loop.config = SimpleNamespace(
        session=SessionConfig(),
        agent=SimpleNamespace(heartbeat=HeartbeatConfig(enabled=False)),
    )
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

    async def fake_process(event, trace_id, publish_response=False, activity=None):
        return ProcessResult(response_text="", outbound_sent=False, degraded_notices=[notice])

    monkeypatch.setattr(loop, "_process_event", fake_process)
    monkeypatch.setattr(loop, "_is_approval_command", lambda t: False)
    await loop._on_inbound(_event())
    assert len(sent) == 1
    assert notice_for(REASON_APPROVAL_UNAVAILABLE) in sent[0].text
    assert sent[0].is_final is True


@pytest.mark.asyncio
async def test_empty_response_no_notice_sends_generic_chinese(monkeypatch):
    loop, sent = _make_loop()

    async def fake_process(event, trace_id, publish_response=False, activity=None):
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

    async def fake_process(event, trace_id, publish_response=False, activity=None):
        return ProcessResult(response_text=english, outbound_sent=False, degraded_notices=[notice])

    monkeypatch.setattr(loop, "_process_event", fake_process)
    monkeypatch.setattr(loop, "_is_approval_command", lambda t: False)
    await loop._on_inbound(_event())
    assert len(sent) == 1
    assert english not in sent[0].text
    assert notice_for(REASON_APPROVAL_UNAVAILABLE) in sent[0].text


@pytest.mark.asyncio
async def test_real_answer_not_yet_sent_combines_answer_and_notice(monkeypatch):
    loop, sent = _make_loop()
    notice = notice_for(REASON_APPROVAL_UNAVAILABLE)

    async def fake_process(event, trace_id, publish_response=False, activity=None):
        return ProcessResult(response_text="真实回答", outbound_sent=False, degraded_notices=[notice])

    monkeypatch.setattr(loop, "_process_event", fake_process)
    monkeypatch.setattr(loop, "_is_approval_command", lambda t: False)
    await loop._on_inbound(_event())
    # answer not yet streamed; convergence point sends answer + notice in one message
    assert len(sent) == 1
    assert "真实回答" in sent[0].text
    assert notice_for(REASON_APPROVAL_UNAVAILABLE) in sent[0].text


@pytest.mark.asyncio
async def test_real_answer_already_sent_appends_notice(monkeypatch):
    loop, sent = _make_loop()
    notice = notice_for(REASON_APPROVAL_UNAVAILABLE)

    async def fake_process(event, trace_id, publish_response=False, activity=None):
        return ProcessResult(response_text="真实回答", outbound_sent=True, degraded_notices=[notice])

    monkeypatch.setattr(loop, "_process_event", fake_process)
    monkeypatch.setattr(loop, "_is_approval_command", lambda t: False)
    await loop._on_inbound(_event())
    # main answer already streamed; notice delivered as a single follow-up
    assert len(sent) == 1
    assert notice_for(REASON_APPROVAL_UNAVAILABLE) in sent[0].text
    assert "真实回答" not in sent[0].text


@pytest.mark.asyncio
async def test_real_answer_no_notice_unchanged(monkeypatch):
    loop, sent = _make_loop()

    async def fake_process(event, trace_id, publish_response=False, activity=None):
        return ProcessResult(response_text="真实回答", outbound_sent=False, degraded_notices=[])

    monkeypatch.setattr(loop, "_process_event", fake_process)
    monkeypatch.setattr(loop, "_is_approval_command", lambda t: False)
    await loop._on_inbound(_event())
    assert len(sent) == 1
    assert sent[0].text == "真实回答"


# --- Approval-path invariants (spec 5.1) -------------------------------------
#
# These lock the Task 2/3 behaviour so a future change cannot silently reroute
# smart `unavailable` back into a blocking manual wait, nor let 2.1 swallow a
# legitimate ESCALATE before it reaches the manual approval flow.


class _FakeInf:
    def needs_confirmation(self, name: str) -> bool:
        return False


def _gate_with_provider(content):
    cfg = load_config()
    cfg.permissions.approval.mode = "smart"
    # "exec" is not in the default require_approval list, and ApprovalManager's
    # default_policy is "approve" — so without this an ESCALATE verdict would be
    # auto-approved inside the manual flow and never publish a request. Force
    # "exec" to require approval so the manual flow yields a PENDING request,
    # which is exactly the path the ESCALATE invariant must exercise.
    cfg.permissions.approval.require_approval = [*cfg.permissions.approval.require_approval, "exec"]
    appr = ApprovalManager(require_approval=cfg.permissions.approval.require_approval)
    provider = MagicMock()
    provider.chat_with_retry = AsyncMock(return_value=MagicMock(content=content))
    return ApprovalGate(config=cfg, approval=appr, inference=_FakeInf(), bus=MessageBus(), provider=provider)


def _exec_event():
    return InboundEvent(
        channel="weixin", sender_id="u1", chat_id="c1",
        content=[ContentBlock(type=ContentType.TEXT, text="research")],
    )


@pytest.mark.asyncio
async def test_smart_unavailable_does_not_block_in_manual():
    # Empty provider content -> unavailable -> immediate denial with notice,
    # NOT a blocking manual-approval wait.
    gate = _gate_with_provider("")
    check = await gate.check("exec", {"command": "curl x"}, "u1", channel="weixin", event=_exec_event())
    assert check.denial is not None
    assert check.notify_user is True
    assert "安全审批暂时不可用" in check.notice


@pytest.mark.asyncio
async def test_smart_escalate_still_enters_manual_flow(monkeypatch):
    # Explicit ESCALATE must still reach the manual flow (publishes a request),
    # i.e. 2.1 must not swallow legitimate escalations.
    gate = _gate_with_provider("ESCALATE")
    published = []
    monkeypatch.setattr(gate._bus, "publish_outbound", AsyncMock(side_effect=lambda o: published.append(o)))
    # wait_for_decision returns None (no decider) quickly to avoid real blocking.
    monkeypatch.setattr(gate._approval, "wait_for_decision", AsyncMock(return_value=None))
    check = await gate.check("exec", {"command": "ls"}, "u1", channel="weixin", event=_exec_event())
    # an approval request was published (manual flow entered)
    assert any(getattr(o, "metadata", {}).get("_approval_request") for o in published)
    # and the gate returns a notify-the-user timeout denial rather than hanging
    assert check.denial is not None
    # The timeout notice must reflect the current copy: the stale request has
    # expired, so the user is told to re-trigger — NOT to /approve the dead id.
    assert check.notify_user is True
    assert check.notice is not None
    assert "超时" in check.notice
    assert "重新发起" in check.notice
    assert "/approve" not in check.notice
    assert gate._approval is not None  # sanity: gate held a real manager
