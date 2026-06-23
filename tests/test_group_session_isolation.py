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


def test_session_config_has_group_scope_default_per_user():
    from echo_agent.config.schema import SessionConfig
    cfg = SessionConfig()
    assert cfg.group_session_scope == "per_user"


def test_group_scope_field_rejects_unknown_value():
    import pytest
    from pydantic import ValidationError
    from echo_agent.config.schema import SessionConfig
    with pytest.raises(ValidationError):
        SessionConfig(group_session_scope="everyone")


def _resolve(scope: str, event: InboundEvent) -> str:
    """复刻 _on_inbound 的解析契约：群聊 per_user 写回 override。"""
    if not event.session_key_override:
        event.session_key_override = event.scoped_session_key(scope)
    return event.session_key


def test_on_inbound_resolution_isolates_group_per_user():
    a = InboundEvent.text_message(channel="telegram", sender_id="alice",
                                  chat_id="grp1", text="x", is_group=True)
    b = InboundEvent.text_message(channel="telegram", sender_id="bob",
                                  chat_id="grp1", text="y", is_group=True)
    assert _resolve("per_user", a) == "telegram:grp1:alice"
    assert _resolve("per_user", b) == "telegram:grp1:bob"


def test_on_inbound_resolution_shared_keeps_single():
    a = InboundEvent.text_message(channel="telegram", sender_id="alice",
                                  chat_id="grp1", text="x", is_group=True)
    assert _resolve("shared", a) == "telegram:grp1"


def test_build_event_sets_is_group_flag():
    from unittest.mock import MagicMock
    from echo_agent.channels.base import BaseChannel

    # BaseChannel is an ABC; subclass with no-op abstract methods to test _build_event.
    class _Ch(BaseChannel):
        async def start(self) -> None: ...
        async def stop(self) -> None: ...
        async def send(self, event):  # noqa: ANN001
            return None

    ch = _Ch.__new__(_Ch)
    ch.config = MagicMock(allow_from=["*"])
    ch.name = "telegram"

    grp = ch._build_event(sender_id="alice", chat_id="grp1", text="hi", is_group=True)
    assert grp.is_group is True
    priv = ch._build_event(sender_id="alice", chat_id="alice", text="hi")
    assert priv.is_group is False


from echo_agent.scheduler.delivery import target_from_session_key


def test_target_strips_group_sender_suffix():
    # 群聊 per_user 键 -> 投递目标是群 chat_id，而非 chat_id:sender
    assert target_from_session_key("telegram:grp1:alice") == ("telegram", "grp1")


def test_target_private_two_part_unchanged():
    assert target_from_session_key("telegram:c1") == ("telegram", "c1")


def test_target_gateway_unchanged():
    assert target_from_session_key("gateway:sess123:user1") == ("gateway:sess123", "user1")


def test_target_empty_or_malformed():
    assert target_from_session_key("") == ("", "")
    assert target_from_session_key("nocolon") == ("", "")


def test_group_per_user_memory_not_cross_visible(tmp_path):
    """群内 alice 写入的 USER 记忆，对 bob 的 per_user 会话键不可见。

    覆盖 scope_policy 非 legacy 的情形（legacy 下 USER 全局可见，隔离为 no-op）。
    """
    from echo_agent.memory.store import MemoryStore
    from echo_agent.memory.types import MemoryEntry, MemoryType, MemoryTier

    alice = InboundEvent.text_message(channel="telegram", sender_id="alice",
                                      chat_id="grp1", text="x", is_group=True)
    bob = InboundEvent.text_message(channel="telegram", sender_id="bob",
                                    chat_id="grp1", text="y", is_group=True)
    a_key = alice.scoped_session_key("per_user")
    b_key = bob.scoped_session_key("per_user")
    assert a_key != b_key  # 隔离键前提成立

    store = MemoryStore(memory_dir=tmp_path / "mem", scope_policy="session")
    entry = MemoryEntry(type=MemoryType.USER, tier=MemoryTier.SEMANTIC,
                        key="pref", content="alice secret", source_session=a_key)

    # per_user 键确实驱动可见性隔离：alice 自己的键可见，bob 的键不可见。
    assert store.is_visible_in_session(entry, a_key) is True
    assert store.is_visible_in_session(entry, b_key) is False
