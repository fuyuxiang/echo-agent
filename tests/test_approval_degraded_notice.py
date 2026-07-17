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
    loop.cognitive_emitter = None
    loop._running = True
    # __new__ bypasses __init__, so the InterruptManager the loop registers each
    # turn (request/clear around the session lock) is not wired up. Provide a
    # real one so these delivery-focused tests exercise the actual turn lifecycle.
    from echo_agent.agent.interrupt_manager import InterruptManager
    loop.interrupt = InterruptManager()
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
    # Convergence now happens inside _process_event (ResponseStage.finalize),
    # so the stub returns the already-converged text; the loop only delivers.
    loop, sent = _make_loop()
    notice = notice_for(REASON_APPROVAL_UNAVAILABLE)

    async def fake_process(event, trace_id, publish_response=False, activity=None):
        return ProcessResult(response_text=notice, outbound_sent=False, degraded_notices=[notice])

    monkeypatch.setattr(loop, "_process_event", fake_process)
    monkeypatch.setattr(loop, "_is_approval_command", lambda t: False)
    await loop._on_inbound(_event())
    assert len(sent) == 1
    assert notice_for(REASON_APPROVAL_UNAVAILABLE) in sent[0].text
    assert sent[0].is_final is True


def test_converge_empty_with_notice_returns_notice():
    from echo_agent.agent.degraded_notice import converge_response_text
    notice = notice_for(REASON_APPROVAL_UNAVAILABLE)
    assert converge_response_text("", [notice]) == notice


def test_converge_empty_no_notice_substitutes_generic_chinese():
    from echo_agent.agent.degraded_notice import (
        GENERIC_FALLBACK_TEXT,
        converge_response_text,
    )
    assert converge_response_text("", [], substitute_empty=True) == GENERIC_FALLBACK_TEXT
    # Inspection/internal rounds keep empty text empty ("no news, stay silent").
    assert converge_response_text("", [], substitute_empty=False) == ""


def test_converge_generic_english_replaced_by_notice():
    from echo_agent.agent.degraded_notice import converge_response_text
    notice = notice_for(REASON_APPROVAL_UNAVAILABLE)
    english = "I encountered an issue processing your request. Please try again or rephrase your question."
    converged = converge_response_text(english, [notice])
    assert english not in converged
    assert notice in converged


def test_converge_generic_english_no_notice_becomes_generic_chinese():
    from echo_agent.agent.degraded_notice import (
        GENERIC_FALLBACK_TEXT,
        converge_response_text,
    )
    english = "I encountered an issue processing your request. Please try again."
    assert converge_response_text(english, []) == GENERIC_FALLBACK_TEXT


def test_converge_real_answer_combines_answer_and_notice():
    from echo_agent.agent.degraded_notice import converge_response_text
    notice = notice_for(REASON_APPROVAL_UNAVAILABLE)
    converged = converge_response_text("真实回答", [notice])
    assert "真实回答" in converged
    assert notice in converged


def test_converge_real_answer_no_notice_unchanged():
    from echo_agent.agent.degraded_notice import converge_response_text
    assert converge_response_text("真实回答", []) == "真实回答"


@pytest.mark.asyncio
async def test_streamed_answer_not_republished(monkeypatch):
    # Converged text was already delivered by the stream publisher
    # (outbound_sent=True) — the loop must not publish a duplicate.
    loop, sent = _make_loop()

    async def fake_process(event, trace_id, publish_response=False, activity=None):
        return ProcessResult(response_text="真实回答", outbound_sent=True, degraded_notices=[])

    monkeypatch.setattr(loop, "_process_event", fake_process)
    monkeypatch.setattr(loop, "_is_approval_command", lambda t: False)
    await loop._on_inbound(_event())
    assert sent == []


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
