from __future__ import annotations

from echo_agent.permissions.manager import ApprovalManager, ApprovalStatus


def test_required_approval_overrides_default_approve() -> None:
    manager = ApprovalManager(require_approval=["exec"], default_policy="approve")

    request = manager.request_approval("exec", tool_name="exec", user_id="u1")

    assert request.status == ApprovalStatus.PENDING
    assert manager.get(request.id) is request


def test_auto_deny_and_default_approve() -> None:
    manager = ApprovalManager(auto_deny=["danger"], default_policy="approve")

    denied = manager.request_approval("danger")
    allowed = manager.request_approval("read_file")

    assert denied.status == ApprovalStatus.DENIED
    assert allowed.status == ApprovalStatus.APPROVED


def test_approved_request_allows_same_call_once() -> None:
    manager = ApprovalManager(require_approval=["exec"], default_policy="approve")
    first = manager.request_approval("exec", tool_name="exec", params={"command": "date"}, user_id="u1")

    assert manager.approve(first.id, decided_by="admin")
    second = manager.request_approval("exec", tool_name="exec", params={"command": "date"}, user_id="u1")
    third = manager.request_approval("exec", tool_name="exec", params={"command": "date"}, user_id="u1")

    assert second.status == ApprovalStatus.APPROVED
    assert third.status == ApprovalStatus.PENDING


def test_approval_state_persists_pending_and_one_time_grant(tmp_path) -> None:
    store_path = tmp_path / "approvals.json"
    manager = ApprovalManager(require_approval=["exec"], default_policy="ask", store_path=store_path)
    first = manager.request_approval("exec", tool_name="exec", params={"command": "date"}, user_id="u1")

    reloaded = ApprovalManager(require_approval=["exec"], default_policy="ask", store_path=store_path)
    pending = reloaded.get_pending()
    assert [req.id for req in pending] == [first.id]

    assert reloaded.approve(first.id, decided_by="admin")

    restarted = ApprovalManager(require_approval=["exec"], default_policy="ask", store_path=store_path)
    approved = restarted.request_approval("exec", tool_name="exec", params={"command": "date"}, user_id="u1")
    next_request = restarted.request_approval("exec", tool_name="exec", params={"command": "date"}, user_id="u1")

    assert approved.status == ApprovalStatus.APPROVED
    assert next_request.status == ApprovalStatus.PENDING
