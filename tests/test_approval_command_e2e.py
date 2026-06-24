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
