"""End-to-end test for /approve command with session/always parameters."""

import pytest

from echo_agent.agent.loop import AgentLoop
from echo_agent.bus.events import InboundEvent
from echo_agent.bus.queue import MessageBus
from echo_agent.config.loader import load_config
from echo_agent.models.provider import LLMProvider, LLMResponse
from echo_agent.permissions.manager import ApprovalStatus


class _StubProvider(LLMProvider):
    """Minimal stub provider for testing."""
    def __init__(self):
        super().__init__()

    async def chat(self, messages, tools=None, model=None, tool_choice=None, **kwargs):
        return LLMResponse(content="ok", finish_reason="stop")

    async def chat_stream(self, messages, tools=None, model=None, tool_choice=None, on_delta=None, **kwargs):
        return await self.chat(messages, tools, model, tool_choice, **kwargs)

    def get_default_model(self):
        return "stub"


def _make_agent_loop(tmp_path):
    """Create an AgentLoop for testing."""
    config = load_config(overrides={"workspace": str(tmp_path)})
    config.permissions.approval.require_approval = ["test_tool"]
    bus = MessageBus()
    provider = _StubProvider()

    loop = AgentLoop(
        bus=bus,
        config=config,
        provider=provider,
        workspace=tmp_path,
    )
    return loop


@pytest.mark.asyncio
class TestApprovalCommandE2E:
    """Test the complete flow from /approve command through to allowlist persistence."""

    async def test_approve_always_persists_to_allowlist(self, tmp_path):
        """When user types '/approve <id> always', it should persist permanently."""
        loop = _make_agent_loop(tmp_path)

        # Create a pending approval request
        req = loop.approval.request_approval("test_tool", "test_tool", {"arg": "value"}, "user1")
        assert req.status == ApprovalStatus.PENDING

        # Simulate user typing: /approve <id> always
        event = InboundEvent.text_message(
            channel="cli",
            chat_id="test_chat",
            sender_id="user1",
            text=f"/approve {req.id} always",
        )

        # Process the command
        reply = await loop._handle_approval_command(event)
        assert f"Approval request {req.id} approved." in reply

        # Verify the request was approved with "always" in reason
        decided = loop.approval._find_history(req.id)
        assert decided is not None
        assert decided.status == ApprovalStatus.APPROVED
        assert decided.reason == "always"

        # Verify it would be stored in allowlist (simulating what approval_gate does)
        level = loop.approval_gate._parse_approval_level(decided.reason)

        from echo_agent.permissions.allowlist import ApprovalLevel
        assert level == ApprovalLevel.ALWAYS

    async def test_approve_session_persists_for_session(self, tmp_path):
        """When user types '/approve <id> session', it should persist for the session."""
        loop = _make_agent_loop(tmp_path)

        # Create a pending approval request
        req = loop.approval.request_approval("test_tool", "test_tool", {"arg": "value"}, "user1")
        assert req.status == ApprovalStatus.PENDING

        # Simulate user typing: /approve <id> session
        event = InboundEvent.text_message(
            channel="cli",
            chat_id="test_chat",
            sender_id="user1",
            text=f"/approve {req.id} session",
        )

        # Process the command
        reply = await loop._handle_approval_command(event)
        assert f"Approval request {req.id} approved." in reply

        # Verify the request was approved with "session" in reason
        decided = loop.approval._find_history(req.id)
        assert decided is not None
        assert decided.status == ApprovalStatus.APPROVED
        assert decided.reason == "session"

        # Verify level parsing
        level = loop.approval_gate._parse_approval_level(decided.reason)

        from echo_agent.permissions.allowlist import ApprovalLevel
        assert level == ApprovalLevel.SESSION

    async def test_approve_without_level_defaults_to_once(self, tmp_path):
        """When user types '/approve <id>' without level, it should default to ONCE."""
        loop = _make_agent_loop(tmp_path)

        # Create a pending approval request
        req = loop.approval.request_approval("test_tool", "test_tool", {"arg": "value"}, "user1")
        assert req.status == ApprovalStatus.PENDING

        # Simulate user typing: /approve <id> (no level)
        event = InboundEvent.text_message(
            channel="cli",
            chat_id="test_chat",
            sender_id="user1",
            text=f"/approve {req.id}",
        )

        # Process the command
        reply = await loop._handle_approval_command(event)
        assert f"Approval request {req.id} approved." in reply

        # Verify the request was approved with empty reason
        decided = loop.approval._find_history(req.id)
        assert decided is not None
        assert decided.status == ApprovalStatus.APPROVED
        assert decided.reason == ""

        # Verify level parsing defaults to ONCE
        level = loop.approval_gate._parse_approval_level(decided.reason)

        from echo_agent.permissions.allowlist import ApprovalLevel
        assert level == ApprovalLevel.ONCE

    async def test_reapprove_already_approved_reports_already_decided(self, tmp_path):
        """A second /approve on an already-approved id must not say 'not found'."""
        loop = _make_agent_loop(tmp_path)
        req = loop.approval.request_approval("test_tool", "test_tool", {"arg": "v"}, "user1")
        event = InboundEvent.text_message(
            channel="cli", chat_id="c", sender_id="user1", text=f"/approve {req.id}",
        )
        first = await loop._handle_approval_command(event)
        assert "approved." in first

        # Same id again — request already left _pending.
        second = await loop._handle_approval_command(event)
        assert "not found" not in second
        assert "already approved" in second
        assert req.id in second

    async def test_reject_already_denied_reports_already_decided(self, tmp_path):
        """A second /deny (or /approve) on a denied id reports the denial, not 'not found'."""
        loop = _make_agent_loop(tmp_path)
        req = loop.approval.request_approval("test_tool", "test_tool", {"arg": "v"}, "user1")
        deny_event = InboundEvent.text_message(
            channel="cli", chat_id="c", sender_id="user1", text=f"/deny {req.id} nope",
        )
        assert "denied." in await loop._handle_approval_command(deny_event)

        approve_event = InboundEvent.text_message(
            channel="cli", chat_id="c", sender_id="user1", text=f"/approve {req.id}",
        )
        reply = await loop._handle_approval_command(approve_event)
        assert "not found" not in reply
        assert "already denied" in reply

    async def test_approve_expired_request_tells_user_to_retrigger(self, tmp_path):
        """An expired request can't be approved; the reply must explain why."""
        from echo_agent.permissions.manager import ApprovalStatus

        loop = _make_agent_loop(tmp_path)
        req = loop.approval.request_approval("test_tool", "test_tool", {"arg": "v"}, "user1")
        # Simulate the timeout path: move it to history as EXPIRED, drop from pending.
        loop.approval._pending.pop(req.id, None)
        req.status = ApprovalStatus.EXPIRED
        loop.approval._history.append(req)

        event = InboundEvent.text_message(
            channel="cli", chat_id="c", sender_id="user1", text=f"/approve {req.id}",
        )
        reply = await loop._handle_approval_command(event)
        assert "not found" not in reply
        assert "expired" in reply

    async def test_approve_truly_unknown_id_still_reports_not_found(self, tmp_path):
        """An id that never existed keeps the original 'not found' message."""
        loop = _make_agent_loop(tmp_path)
        event = InboundEvent.text_message(
            channel="cli", chat_id="c", sender_id="user1", text="/approve deadbeef00",
        )
        reply = await loop._handle_approval_command(event)
        assert "not found" in reply
        assert "deadbeef00" in reply

    async def test_redeny_already_denied_without_reason_omits_suffix(self, tmp_path):
        """Denied with no reason: the description must not dangle a ': ' suffix."""
        loop = _make_agent_loop(tmp_path)
        req = loop.approval.request_approval("test_tool", "test_tool", {"arg": "v"}, "user1")
        deny_event = InboundEvent.text_message(
            channel="cli", chat_id="c", sender_id="user1", text=f"/deny {req.id}",
        )
        assert "denied." in await loop._handle_approval_command(deny_event)

        # Re-deny the same id — it has left _pending, so we get the history description.
        second = await loop._handle_approval_command(deny_event)
        assert "not found" not in second
        assert "already denied" in second
        # Empty reason -> no trailing "reason" clause.
        assert second.rstrip().endswith("already denied.")
        assert ":" not in second.split("already denied", 1)[1]

    async def test_inactive_approval_description_visible_across_users(self, tmp_path):
        """With no admin_users configured, a non-owner still gets the historic
        description (the inactive-approval lookup runs before the owner check)."""
        loop = _make_agent_loop(tmp_path)
        assert not loop.config.permissions.admin_users  # precondition for this branch
        req = loop.approval.request_approval("test_tool", "test_tool", {"arg": "v"}, "owner")
        loop.approval.approve(req.id, decided_by="owner")

        other = InboundEvent.text_message(
            channel="cli", chat_id="c", sender_id="someone_else", text=f"/approve {req.id}",
        )
        reply = await loop._handle_approval_command(other)
        assert "already approved" in reply
        assert "not allowed" not in reply

    async def test_pending_request_owner_check_blocks_non_owner(self, tmp_path):
        """A still-pending request owned by someone else is gated by the owner check,
        not surfaced as an inactive-approval description."""
        loop = _make_agent_loop(tmp_path)
        req = loop.approval.request_approval("test_tool", "test_tool", {"arg": "v"}, "owner")

        other = InboundEvent.text_message(
            channel="cli", chat_id="c", sender_id="intruder", text=f"/approve {req.id}",
        )
        reply = await loop._handle_approval_command(other)
        assert "not allowed to decide" in reply
        # Untouched: still pending, never approved by the intruder.
        assert loop.approval.get(req.id) is not None

    async def test_approve_race_lost_falls_back_to_description(self, tmp_path):
        """check-then-act guard: if the request is decided between get() and
        approve() (a TOCTOU window only reachable if a future await is inserted
        there), approve() returns False and we describe its historic state rather
        than silently dropping the command. We force the lost-race outcome by
        stubbing approve() to False while moving the request into history."""
        loop = _make_agent_loop(tmp_path)
        req = loop.approval.request_approval("test_tool", "test_tool", {"arg": "v"}, "user1")

        def _lose_race(request_id, level="", decided_by=""):
            # Simulate a concurrent decider winning: pop pending, record as approved.
            loop.approval._pending.pop(request_id, None)
            req.status = ApprovalStatus.APPROVED
            req.decided_at = "2026-06-28T00:00:00"
            loop.approval._history.append(req)
            return False

        loop.approval.approve = _lose_race
        event = InboundEvent.text_message(
            channel="cli", chat_id="c", sender_id="user1", text=f"/approve {req.id}",
        )
        reply = await loop._handle_approval_command(event)
        assert "approved." != reply  # not the happy-path confirmation
        assert "already approved" in reply
        assert req.id in reply

    async def test_deny_race_lost_falls_back_to_description(self, tmp_path):
        """Same check-then-act guard for the /deny branch."""
        loop = _make_agent_loop(tmp_path)
        req = loop.approval.request_approval("test_tool", "test_tool", {"arg": "v"}, "user1")

        def _lose_race(request_id, reason="", decided_by=""):
            loop.approval._pending.pop(request_id, None)
            req.status = ApprovalStatus.DENIED
            req.reason = "raced"
            loop.approval._history.append(req)
            return False

        loop.approval.deny = _lose_race
        event = InboundEvent.text_message(
            channel="cli", chat_id="c", sender_id="user1", text=f"/deny {req.id}",
        )
        reply = await loop._handle_approval_command(event)
        assert "already denied" in reply
        assert "raced" in reply
