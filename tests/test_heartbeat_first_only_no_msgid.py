"""Regression for task #2: first_only de-dup must work on channels that return
no platform message_id (e.g. weixin). Before the fix _heartbeat_send only
recorded the turn key when result.message_id was truthy, so weixin (which sends
successfully but returns no id) never recorded the key — and first_only
degraded into sending a fresh "正在处理中…" on EVERY beat.
"""

import pytest

from echo_agent.bus.events import OutboundEvent
from echo_agent.bus.queue import MessageBus
from echo_agent.channels.base import SendResult
from echo_agent.channels.manager import ChannelManager
from echo_agent.config.schema import ChannelsConfig


class _NoIdChannel:
    """Uneditable channel that succeeds without returning a message_id (weixin)."""

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


def _hb(text, key="evt1"):
    out = OutboundEvent.text_reply(channel="weixin", chat_id="c1", text=text)
    out.is_final = False
    out.message_kind = "heartbeat"
    out.metadata = {"_heartbeat": True, "_inbound_event_id": key,
                    "_hb_on_uneditable": "first_only"}
    return out


@pytest.mark.asyncio
async def test_first_only_dedups_without_message_id():
    mgr = ChannelManager(ChannelsConfig(), MessageBus())
    ch = _NoIdChannel()
    mgr._channels["weixin"] = ch

    await mgr._filter_and_dispatch(_hb("hb1"))
    await mgr._filter_and_dispatch(_hb("hb2"))
    await mgr._filter_and_dispatch(_hb("hb3"))

    assert ch.sent == ["hb1"], f"first_only sent more than once: {ch.sent}"
    assert ch.typings >= 3  # typing still refreshed on every beat
