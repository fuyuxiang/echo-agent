"""Task 8: capability-based heartbeat tiering across channels.

Four tiers exercised here:
- editable channel (telegram-like): first milestone sends one draft, later
  milestones edit that single draft in place.
- async channel (email-like, is_realtime=False): zero heartbeat.
- plain-text channel (weixin-like) in key_milestones verbosity: only key
  milestones are emitted; non-key mid-run milestones are suppressed.
"""

from __future__ import annotations

import pytest

from echo_agent.bus.events import OutboundEvent
from echo_agent.bus.queue import MessageBus
from echo_agent.channels.base import SendResult
from echo_agent.channels.manager import ChannelManager
from echo_agent.config.schema import ChannelsConfig, HeartbeatConfig


class _EditableChannel:
    name = "telegram"
    supports_edit = True
    supports_reactions = True
    is_realtime = True

    def __init__(self):
        self.sent, self.edited = [], []
        self.config = type("C", (), {"reactions_enabled": False})()

    async def send(self, event):
        self.sent.append(event.text)
        return SendResult(success=True, message_id="m1")

    async def edit_message(self, chat_id, message_id, text, *, metadata=None, finalize=False):
        self.edited.append((message_id, text))
        return SendResult(success=True, message_id=message_id)

    async def send_typing(self, chat_id, metadata=None):
        pass


class _FakePlainChannel:
    """Uneditable channel (weixin-like): send succeeds without a message_id.

    Mirrors the helper in test_heartbeat_milestone_dedup; defined locally because
    the tests dir is not an importable package (no __init__.py), so a cross-test
    `from tests.test_heartbeat_milestone_dedup import ...` does not resolve.
    """

    name = "weixin"
    supports_edit = False
    supports_reactions = False
    is_realtime = True

    def __init__(self):
        self.sent = []
        self.config = type("C", (), {"reactions_enabled": False})()

    async def send(self, event):
        self.sent.append(event.text)
        return SendResult(success=True)  # weixin returns no message_id

    async def send_typing(self, chat_id, metadata=None):
        pass


class _AsyncChannel:
    name = "email"
    supports_edit = False
    supports_reactions = False
    is_realtime = False

    def __init__(self):
        self.sent = []
        self.config = type("C", (), {"reactions_enabled": False})()

    async def send(self, event):
        self.sent.append(event.text)
        return SendResult(success=True)

    async def send_typing(self, chat_id, metadata=None):
        pass


@pytest.fixture
def manager_with_channel():
    """Build a ChannelManager wired to a fake channel, with a chosen verbosity."""

    def _build(channel, *, verbosity: str = "key_milestones"):
        mgr = ChannelManager(ChannelsConfig(), MessageBus())
        mgr._channels[channel.name] = channel
        mgr._heartbeat_cfg = HeartbeatConfig(verbosity=verbosity)
        return mgr, channel

    return _build


def _hb(channel, milestone, text):
    ev = OutboundEvent.text_reply(channel=channel, chat_id="u1", text=text,
                                  is_final=False, message_kind="heartbeat")
    ev.metadata = {"_heartbeat": True, "_inbound_event_id": "t1", "_hb_milestone": milestone}
    return ev


@pytest.mark.asyncio
async def test_editable_channel_edits_single_message(manager_with_channel):
    mgr, ch = manager_with_channel(_EditableChannel(), verbosity="every_tool")
    await mgr._handle_heartbeat(_hb("telegram", 1, "step1"))
    await mgr._handle_heartbeat(_hb("telegram", 2, "step2"))
    assert len(ch.sent) == 1          # first milestone -> one send
    assert len(ch.edited) == 1        # subsequent milestone -> edit, not new send


@pytest.mark.asyncio
async def test_async_channel_gets_zero_heartbeat(manager_with_channel):
    mgr, ch = manager_with_channel(_AsyncChannel(), verbosity="every_tool")
    await mgr._handle_heartbeat(_hb("email", 1, "step1"))
    await mgr._handle_heartbeat(_hb("email", 2, "step2"))
    assert ch.sent == []              # async tier: zero heartbeat


@pytest.mark.asyncio
async def test_key_milestones_suppresses_non_key(manager_with_channel):
    # In key_milestones verbosity, a plain-text channel only emits for key
    # milestones (first tool entry / finalize). A mid-run tool-done milestone
    # without the key flag is suppressed.
    mgr, ch = manager_with_channel(_FakePlainChannel(), verbosity="key_milestones")
    ev = _hb("weixin", 1, "step")
    ev.metadata["_hb_key"] = False
    await mgr._handle_heartbeat(ev)
    assert ch.sent == []
    ev2 = _hb("weixin", 2, "key-step")
    ev2.metadata["_hb_key"] = True
    await mgr._handle_heartbeat(ev2)
    assert ch.sent == ["key-step"]
