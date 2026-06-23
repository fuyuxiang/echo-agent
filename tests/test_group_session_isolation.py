# tests/test_group_session_isolation.py
from __future__ import annotations

from echo_agent.bus.events import InboundEvent


def _evt(sender_id: str, chat_id: str, is_group: bool) -> InboundEvent:
    return InboundEvent.text_message(
        channel="telegram", sender_id=sender_id, chat_id=chat_id,
        text="hi", is_group=is_group,
    )


def test_private_chat_key_never_includes_sender():
    evt = _evt("u1", "c1", is_group=False)
    assert evt.scoped_session_key("per_user") == "telegram:c1"
    assert evt.scoped_session_key("shared") == "telegram:c1"


def test_group_per_user_splits_by_sender():
    a = _evt("alice", "grp1", is_group=True)
    b = _evt("bob", "grp1", is_group=True)
    assert a.scoped_session_key("per_user") == "telegram:grp1:alice"
    assert b.scoped_session_key("per_user") == "telegram:grp1:bob"
    assert a.scoped_session_key("per_user") != b.scoped_session_key("per_user")


def test_group_shared_keeps_single_key():
    a = _evt("alice", "grp1", is_group=True)
    b = _evt("bob", "grp1", is_group=True)
    assert a.scoped_session_key("shared") == "telegram:grp1"
    assert b.scoped_session_key("shared") == "telegram:grp1"


def test_group_per_user_empty_sender_falls_back():
    evt = _evt("", "grp1", is_group=True)
    assert evt.scoped_session_key("per_user") == "telegram:grp1"


def test_override_wins_over_scope():
    evt = InboundEvent.text_message(
        channel="telegram", sender_id="alice", chat_id="grp1",
        text="hi", is_group=True, session_key_override="custom:key",
    )
    assert evt.scoped_session_key("per_user") == "custom:key"
