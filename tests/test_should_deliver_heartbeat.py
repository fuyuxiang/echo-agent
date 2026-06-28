"""Regression tests for BaseChannel.should_deliver heartbeat passthrough.

Background: heartbeat OutboundEvents carry is_final=False on uneditable channels
(e.g. weixin). The base should_deliver guard exists to suppress token-stream /
progress chunks on channels that cannot edit, but it was also silently dropping
heartbeats — so long-running turns produced no "still working" message on weixin
until the final answer arrived. These tests pin the passthrough so the
ChannelManager's on_uneditable strategy stays authoritative.
"""

from __future__ import annotations

import pytest

from echo_agent.bus.events import OutboundEvent
from echo_agent.bus.queue import MessageBus
from echo_agent.channels.base import BaseChannel, SendResult
from echo_agent.channels.manager import ChannelManager
from echo_agent.config.schema import ChannelsConfig


class _MinimalChannel(BaseChannel):
    """Uneditable channel that, like the real weixin adapter, gates send() on
    should_deliver before touching any platform API."""

    name = "minimal"
    supports_edit = False

    def __init__(self):
        self.delivered: list[OutboundEvent] = []
        self._running = True
        self.config = type("C", (), {"reactions_enabled": False})()

    async def start(self) -> None:  # pragma: no cover - not exercised
        pass

    async def stop(self) -> None:  # pragma: no cover - not exercised
        pass

    async def send(self, event: OutboundEvent) -> SendResult | None:
        if not self.should_deliver(event):
            return SendResult(success=True, skipped=True)
        self.delivered.append(event)
        return SendResult(success=True, message_id=f"m{len(self.delivered)}")

    async def send_typing(self, chat_id, metadata=None) -> None:
        pass

    async def stop_typing(self, chat_id) -> None:
        pass


def _heartbeat_event(text: str = "正在处理中…") -> OutboundEvent:
    out = OutboundEvent.text_reply(channel="minimal", chat_id="c1", text=text)
    out.is_final = False
    out.message_kind = "heartbeat"
    return out


def _progress_event(text: str = "thinking…") -> OutboundEvent:
    out = OutboundEvent.text_reply(channel="minimal", chat_id="c1", text=text)
    out.is_final = False
    out.message_kind = "progress"
    return out


def test_uneditable_channel_delivers_heartbeat():
    ch = _MinimalChannel()
    assert ch.should_deliver(_heartbeat_event()) is True


def test_uneditable_channel_still_drops_non_final_non_heartbeat():
    """The guard must keep suppressing token-stream / progress chunks — only
    heartbeats get the new exemption."""
    ch = _MinimalChannel()
    assert ch.should_deliver(_progress_event()) is False


def test_uneditable_channel_delivers_final():
    ch = _MinimalChannel()
    final = OutboundEvent.text_reply(channel="minimal", chat_id="c1", text="answer")
    final.is_final = True
    final.message_kind = "final"
    assert ch.should_deliver(final) is True


@pytest.mark.asyncio
async def test_heartbeat_reaches_send_on_uneditable_channel():
    """End-to-end through the send() guard the real adapter uses: a heartbeat
    must actually be delivered (not skipped) on an uneditable channel."""
    ch = _MinimalChannel()
    result = await ch.send(_heartbeat_event())
    assert result is not None and not result.skipped
    assert [e.text for e in ch.delivered] == ["正在处理中…"]


@pytest.mark.asyncio
async def test_first_only_strategy_delivers_one_heartbeat_via_manager():
    """The manager's first_only strategy plus the relaxed guard yields exactly
    one heartbeat on an uneditable channel — not zero (the bug) and not many."""
    manager = ChannelManager(ChannelsConfig(), MessageBus())
    ch = _MinimalChannel()
    manager._channels["minimal"] = ch

    first = _heartbeat_event("hb1")
    first.metadata = {"_heartbeat": True, "_inbound_event_id": "evt1",
                      "_hb_on_uneditable": "first_only"}
    second = _heartbeat_event("hb2")
    second.metadata = {"_heartbeat": True, "_inbound_event_id": "evt1",
                       "_hb_on_uneditable": "first_only"}

    await manager._filter_and_dispatch(first)
    await manager._filter_and_dispatch(second)

    assert [e.text for e in ch.delivered] == ["hb1"]
