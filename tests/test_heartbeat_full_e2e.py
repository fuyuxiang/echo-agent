import asyncio
import time

import pytest

from echo_agent.agent.progress_heartbeat import ProgressHeartbeat, SharedActivityState
from echo_agent.bus.events import InboundEvent, OutboundEvent
from echo_agent.bus.queue import MessageBus
from echo_agent.channels.base import SendResult
from echo_agent.channels.manager import ChannelManager
from echo_agent.config.schema import ChannelsConfig, HeartbeatConfig


class _Chan:
    def __init__(self):
        self.supports_edit = True
        self.is_running = True
        self.config = type("C", (), {"reactions_enabled": False})()
        self.sent, self.edits = [], []
        self._n = 0

    async def send(self, e):
        self._n += 1
        self.sent.append(e.text)
        return SendResult(success=True, message_id=f"m{self._n}")

    async def edit_message(self, chat_id, mid, text, *, metadata=None, finalize=False):
        self.edits.append(text)
        return SendResult(success=True, message_id=mid)

    async def send_typing(self, chat_id, metadata=None):
        pass

    async def stop_typing(self, chat_id):
        pass


@pytest.mark.asyncio
async def test_heartbeats_precede_final_and_seal():
    bus = MessageBus()
    mgr = ChannelManager(ChannelsConfig(), bus)
    ch = _Chan()
    mgr._channels["cli"] = ch
    ev = InboundEvent(channel="cli", chat_id="c1")
    # interval_sec must satisfy schema ge=1; first_delay_sec=0 makes the
    # first beat fire immediately so a short test window still produces one.
    hb = ProgressHeartbeat(bus, ev, HeartbeatConfig(first_delay_sec=0, interval_sec=1))
    activity = SharedActivityState(started_at=time.monotonic())

    await hb.start(activity)
    await asyncio.sleep(0.1)          # long work -> beats flow through bus -> manager
    await hb.stop()                   # final answer arrives -> seal
    beats_at_seal = len(ch.sent) + len(ch.edits)

    # deliver final answer through the manager
    final = OutboundEvent.text_reply(channel="cli", chat_id="c1", text="最终答案")
    final.is_final = True
    final.message_kind = "final"
    final.metadata = {"_inbound_event_id": ev.event_id}
    await mgr._filter_and_dispatch(final)

    await asyncio.sleep(0.1)
    assert beats_at_seal >= 1                      # heartbeat happened before final
    assert len(ch.sent) == 1                       # editable channel: one message slot total
    # final answer seals into that same message (edited in place on editable channels)
    assert "最终答案" in (ch.edits[-1] if ch.edits else "") or any(
        "最终答案" in t for t in ch.sent
    )
