from __future__ import annotations

from echo_agent.bus.events import InboundEvent, ContentBlock, ContentType


def _ev(channel, chat_id, sender_id, is_group=False):
    return InboundEvent(
        channel=channel, chat_id=chat_id, sender_id=sender_id,
        content=[ContentBlock(type=ContentType.TEXT, text="hi")],
        is_group=is_group,
    )


def test_bound_dm_maps_to_owner():
    ev = _ev("telegram", "alice", "alice")
    scope = ev.memory_scope_key("shared", "owner", {"telegram:alice"})
    assert scope == "owner"


def test_unbound_dm_isolated_to_session():
    ev = _ev("telegram", "bob", "bob")
    scope = ev.memory_scope_key("shared", "owner", {"telegram:alice"})
    assert scope == ev.session_key  # 表外私聊 fail-closed 按会话隔离


def test_group_never_maps_to_owner():
    ev = _ev("slack", "C123", "alice", is_group=True)
    scope = ev.memory_scope_key("per_user", "owner", {"slack:alice"})
    assert scope != "owner"
    assert scope == ev.scoped_session_key("per_user")


def test_empty_bindings_all_isolated():
    ev = _ev("telegram", "alice", "alice")
    assert ev.memory_scope_key("shared", "owner", set()) == ev.session_key
