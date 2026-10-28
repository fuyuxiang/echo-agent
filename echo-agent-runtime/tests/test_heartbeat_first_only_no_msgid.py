"""Regression for task #2 (migrated to the milestone model): dedup must work on
channels that return no platform message_id (e.g. weixin). The plain-text tier
no longer keys off a returned msg id — it dedups by the turn's milestone seq —
so repeat beats for the same milestone collapse to a single send even when the
channel reports success without an id.
"""

import pytest

from echo_agent.bus.events import OutboundEvent
from echo_agent.bus.queue import MessageBus
from echo_agent.channels.base import SendResult
from echo_agent.channels.manager import ChannelManager
from echo_agent.config.schema import ChannelsConfig, HeartbeatConfig


class _NoIdChannel:
    """Uneditable channel that succeeds without returning a message_id (weixin)."""

    name = "weixin"
    supports_edit = False
    is_running = True

    def __init__(self):
        self.config = type("C", (), {"reactions_enabled": False})()
        self.sent = []
        self.typings = 0

    async def send(self, event):
        self.sent.append(event.text)
        return SendResult(success=True)  # note: no message_id

    async def send_typing(self, chat_id, metadata=None):
        self.typings += 1

    async def stop_typing(self, chat_id):
        pass


def _hb(text, key="evt1", milestone=1):
    out = OutboundEvent.text_reply(channel="weixin", chat_id="c1", text=text)
    out.is_final = False
    out.message_kind = "heartbeat"
    out.metadata = {"_heartbeat": True, "_inbound_event_id": key,
                    "_hb_milestone": milestone, "_hb_key": False}
    return out


@pytest.mark.asyncio
async def test_same_milestone_dedups_without_message_id():
    mgr = ChannelManager(ChannelsConfig(), MessageBus())
    mgr._heartbeat_cfg = HeartbeatConfig(verbosity="every_tool")
    ch = _NoIdChannel()
    mgr._channels["weixin"] = ch

    # Three beats for the same milestone seq (the old timer would re-fire).
    await mgr._filter_and_dispatch(_hb("hb1"))
    await mgr._filter_and_dispatch(_hb("hb2"))
    await mgr._filter_and_dispatch(_hb("hb3"))

    assert ch.sent == ["hb1"], f"milestone dedup sent more than once: {ch.sent}"
    assert ch.typings >= 3  # typing still refreshed on every beat
