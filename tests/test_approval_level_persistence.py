"""Test that /approve command with session/always parameters works correctly."""

from echo_agent.permissions.manager import ApprovalManager, ApprovalStatus
from echo_agent.permissions.allowlist import ApprovalAllowlist, ApprovalLevel


class TestApprovalLevelPersistence:
    """Test that approval level (session/always) is correctly passed through the stack."""

    def test_approve_without_level_is_once(self, tmp_path):
        """When /approve is called without level parameter, it should be ONCE (no persistence)."""
        manager = ApprovalManager(
            require_approval=["test_tool"],
            store_path=tmp_path / "approvals.json",
        )

        # Create a pending request
        req = manager.request_approval("test_tool", "test_tool", {"arg": "value"}, "user1")
        assert req.status == ApprovalStatus.PENDING

        # Approve without level (empty string)
        ok = manager.approve(req.id, level="", decided_by="user1")
        assert ok

        # Check that reason is empty, which will be parsed as ONCE
        history = manager.get_pending()
        assert len(history) == 0  # moved to history

        # Simulate what approval_gate does
        decided = manager._find_history(req.id)
        assert decided is not None
        assert decided.reason == ""  # empty means ONCE

    def test_approve_with_session_level(self, tmp_path):
        """When /approve is called with 'session', it should persist for the session."""
        manager = ApprovalManager(
            require_approval=["test_tool"],
            store_path=tmp_path / "approvals.json",
        )
        allowlist = ApprovalAllowlist(store_path=tmp_path / "allowlist.json")

        # Create a pending request
        req = manager.request_approval("test_tool", "test_tool", {"arg": "value"}, "user1")
        assert req.status == ApprovalStatus.PENDING

        # Approve with session level
        ok = manager.approve(req.id, level="session", decided_by="user1")
        assert ok

        # Check that reason contains "session"
        decided = manager._find_history(req.id)
        assert decided is not None
        assert decided.reason == "session"

        # Simulate what approval_gate does: parse level and store in allowlist
        level = ApprovalLevel.SESSION if decided.reason.lower() == "session" else ApprovalLevel.ONCE
        allowlist.approve("session_key_1", "pattern_key_1", level)

        # Verify it's stored in session allowlist
        assert allowlist.is_approved("session_key_1", "pattern_key_1")

    def test_approve_with_always_level(self, tmp_path):
        """When /approve is called with 'always', it should persist permanently."""
        manager = ApprovalManager(
            require_approval=["test_tool"],
            store_path=tmp_path / "approvals.json",
        )
        allowlist = ApprovalAllowlist(store_path=tmp_path / "allowlist.json")

        # Create a pending request
        req = manager.request_approval("test_tool", "test_tool", {"arg": "value"}, "user1")
        assert req.status == ApprovalStatus.PENDING

        # Approve with always level
        ok = manager.approve(req.id, level="always", decided_by="user1")
        assert ok

        # Check that reason contains "always"
        decided = manager._find_history(req.id)
        assert decided is not None
        assert decided.reason == "always"

        # Simulate what approval_gate does: parse level and store in allowlist
        level = ApprovalLevel.ALWAYS if decided.reason.lower() == "always" else ApprovalLevel.ONCE
        allowlist.approve("session_key_1", "pattern_key_1", level)

        # Verify it's stored in permanent allowlist
        assert allowlist.is_approved("session_key_1", "pattern_key_1")

        # Verify it persists across sessions
        assert allowlist.is_approved("session_key_2", "pattern_key_1")

        # Verify it was saved to disk
        allowlist2 = ApprovalAllowlist(store_path=tmp_path / "allowlist.json")
        assert allowlist2.is_approved("session_key_3", "pattern_key_1")

    def test_approve_session_does_not_cross_sessions(self, tmp_path):
        """Session-level approval should not work in a different session."""
        allowlist = ApprovalAllowlist(store_path=tmp_path / "allowlist.json")

        # Approve for session_1
        allowlist.approve("session_1", "pattern_key_1", ApprovalLevel.SESSION)

        # Check it works in session_1
        assert allowlist.is_approved("session_1", "pattern_key_1")

        # Check it doesn't work in session_2
        assert not allowlist.is_approved("session_2", "pattern_key_1")
