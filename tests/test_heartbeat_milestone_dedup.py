"""Task 7: weixin spam reproduction + milestone-based heartbeat dedup.

Reproduces the production bug where a long-running turn floods an uneditable
channel (weixin) with ~10 "正在处理中" messages, then asserts the fix: each turn
delivers a given milestone seq at most once.
"""

from __future__ import annotations

import pytest

from echo_agent.bus.events import OutboundEvent
from echo_agent.bus.queue import MessageBus
from echo_agent.channels.manager import ChannelManager
from echo_agent.config.schema import ChannelsConfig, HeartbeatConfig


class _FakePlainChannel:
    """Uneditable channel (weixin-like): send succeeds without a message_id."""

    name = "weixin"
    supports_edit = False
    supports_reactions = False
    is_realtime = True

    def __init__(self):
        self.sent = []
        self.config = type("C", (), {"reactions_enabled": False})()

    async def send(self, event):
        from echo_agent.channels.base import SendResult

        self.sent.append(event.text)
        return SendResult(success=True)  # weixin returns no message_id

    async def send_typing(self, chat_id, metadata=None):
        pass


def _hb_event(milestone: int, text: str):
    ev = OutboundEvent.text_reply(
        channel="weixin", chat_id="u1", text=text, is_final=False,
        message_kind="heartbeat",
    )
    ev.metadata = {
        "_heartbeat": True,
        "_inbound_event_id": "turn-1",
        "_hb_milestone": milestone,
    }
    return ev


@pytest.fixture
def manager_with_channel():
    """Build a ChannelManager wired to a fake channel, with a chosen verbosity."""

    def _build(channel, *, verbosity: str = "key_milestones"):
        mgr = ChannelManager(ChannelsConfig(), MessageBus())
        mgr._channels[channel.name] = channel
        mgr._heartbeat_cfg = HeartbeatConfig(verbosity=verbosity)
        return mgr, channel

    return _build


@pytest.mark.asyncio
async def test_same_turn_same_milestone_sends_once(manager_with_channel):
    # manager_with_channel: helper building a ChannelManager wired to the fake
    # channel with verbosity="every_tool".
    mgr, ch = manager_with_channel(_FakePlainChannel(), verbosity="every_tool")
    # Three beats for milestone 1 (the old timer would fire repeatedly).
    for _ in range(3):
        await mgr._handle_heartbeat(_hb_event(1, "⏳ 正在查阅资料（已用时 1 分钟）"))
    assert len(ch.sent) == 1  # deduped by milestone, not spammed


@pytest.mark.asyncio
async def test_new_milestones_each_send_once_in_every_tool(manager_with_channel):
    mgr, ch = manager_with_channel(_FakePlainChannel(), verbosity="every_tool")
    await mgr._handle_heartbeat(_hb_event(1, "step1"))
    await mgr._handle_heartbeat(_hb_event(1, "step1"))  # dup
    await mgr._handle_heartbeat(_hb_event(2, "step2"))
    await mgr._handle_heartbeat(_hb_event(2, "step2"))  # dup
    assert ch.sent == ["step1", "step2"]


@pytest.mark.asyncio
async def test_silent_verbosity_sends_no_text(manager_with_channel):
    mgr, ch = manager_with_channel(_FakePlainChannel(), verbosity="silent")
    await mgr._handle_heartbeat(_hb_event(1, "step1"))
    await mgr._handle_heartbeat(_hb_event(2, "step2"))
    assert ch.sent == []
