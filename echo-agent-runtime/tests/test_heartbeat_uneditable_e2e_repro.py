import asyncio
import time

import pytest

from echo_agent.agent.progress_heartbeat import ProgressHeartbeat, SharedActivityState
from echo_agent.bus.events import InboundEvent
from echo_agent.bus.queue import MessageBus
from echo_agent.channels.base import BaseChannel, SendResult
from echo_agent.channels.manager import ChannelManager
from echo_agent.config.schema import ChannelsConfig, HeartbeatConfig


class _UneditableChan:
    """Mimics weixin: supports_edit=False, reuses base should_deliver."""

    name = "weixin"
    supports_edit = False
    is_running = True

    def __init__(self):
        self.config = type("C", (), {"reactions_enabled": False})()
        self.sent = []
        self.typings = 0

    should_deliver = BaseChannel.should_deliver

    async def send(self, event):
        if not self.should_deliver(event):
            return SendResult(success=True, skipped=True)
        self.sent.append(event.text)
        return SendResult(success=True, message_id="m1")

    async def send_typing(self, chat_id, metadata=None):
        self.typings += 1

    async def stop_typing(self, chat_id):
        pass


@pytest.mark.asyncio
async def test_uneditable_channel_receives_heartbeat_through_bus():
    bus = MessageBus()
    mgr = ChannelManager(ChannelsConfig(), bus)
    ch = _UneditableChan()
    mgr._channels["weixin"] = ch
    # every_tool verbosity so the plain-text tier emits every milestone, not just
    # key ones — this test asserts the beat reaches weixin at all.
    mgr._heartbeat_cfg = HeartbeatConfig(verbosity="every_tool")
    ev = InboundEvent(channel="weixin", chat_id="c1")
    hb = ProgressHeartbeat(bus, ev, HeartbeatConfig(first_delay_sec=0, min_interval_sec=1,
                                                    verbosity="every_tool"))
    activity = SharedActivityState(started_at=time.monotonic())
    # A real milestone advances (first tool entry) so a beat is due and flows
    # bus -> manager -> the uneditable channel's send().
    activity.enter_tool("web_search")

    await hb.start(activity)
    # Poll for the beat to propagate bus -> manager -> channel rather than
    # trusting a fixed 150ms window, which can miss on a loaded CI runner. The
    # 2s ceiling only bounds a real failure; normally this resolves in ~1ms.
    for _ in range(200):
        if ch.sent:
            break
        await asyncio.sleep(0.01)
    await hb.stop()

    assert len(ch.sent) >= 1, f"no heartbeat reached weixin; sent={ch.sent} typings={ch.typings}"
