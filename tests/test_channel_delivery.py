"""Tests for ChannelManager delivery logic."""

from __future__ import annotations

from unittest.mock import AsyncMock, MagicMock

import pytest

from echo_agent.bus.events import OutboundEvent, ContentBlock, ContentType
from echo_agent.bus.queue import MessageBus
from echo_agent.channels.manager import ChannelManager
from echo_agent.config.schema import ChannelsConfig


def _final_event(text: str, *, channel: str = "test") -> OutboundEvent:
    event = OutboundEvent(
        channel=channel,
        chat_id="chat-1",
        content=[ContentBlock(type=ContentType.TEXT, text=text)],
        reply_to_id="reply-1",
    )
    event.is_final = True
    event.message_kind = "final"
    event.metadata = {"_inbound_event_id": "inbound-1"}
    return event


class _FakeChannel:
    def __init__(self, name: str):
        self.name = name
        self.sent: list[OutboundEvent] = []
        self.is_running = True
        self.supports_edit = False
        self.config = MagicMock(reactions_enabled=False)

    async def send(self, event: OutboundEvent):
        self.sent.append(event)
        return MagicMock(success=True)

    async def send_typing(self, *a, **kw):
        pass

    async def send_read_receipt(self, *a, **kw):
        pass


@pytest.mark.asyncio
async def test_deliver_final_sends_to_channel() -> None:
    bus = MessageBus()
    manager = ChannelManager(ChannelsConfig(), bus)
    channel = _FakeChannel("mychan")
    manager._channels["mychan"] = channel

    event = _final_event("hello", channel="mychan")
    await manager._deliver_final(event)

    assert len(channel.sent) == 1
    assert channel.sent[0].content[0].text == "hello"


@pytest.mark.asyncio
async def test_deliver_final_no_channel_no_drop() -> None:
    bus = MessageBus()
    manager = ChannelManager(ChannelsConfig(), bus)

    event = _final_event("hello", channel="gateway:api")
    await manager._deliver_final(event)

    assert "_drop" not in event.metadata


@pytest.mark.asyncio
async def test_deliver_final_empty_content_sets_drop() -> None:
    bus = MessageBus()
    manager = ChannelManager(ChannelsConfig(), bus)
    channel = _FakeChannel("mychan")
    manager._channels["mychan"] = channel

    event = _final_event("", channel="mychan")
    await manager._deliver_final(event)

    assert event.metadata.get("_drop") is True
    assert len(channel.sent) == 0


@pytest.mark.asyncio
async def test_filter_dispatch_progress_dropped_when_disabled() -> None:
    bus = MessageBus()
    manager = ChannelManager(ChannelsConfig(), bus)
    manager._send_progress = False

    event = OutboundEvent(
        channel="test",
        chat_id="chat-1",
        content=[ContentBlock(type=ContentType.TEXT, text="thinking...")],
    )
    event.metadata = {"_progress": True}
    event.is_final = False

    await manager._filter_and_dispatch(event)

    assert event.metadata.get("_drop") is True


@pytest.mark.asyncio
async def test_filter_dispatch_token_stream_handled() -> None:
    bus = MessageBus()
    manager = ChannelManager(ChannelsConfig(), bus)
    channel = _FakeChannel("mychan")
    manager._channels["mychan"] = channel

    event = OutboundEvent(
        channel="mychan",
        chat_id="chat-1",
        content=[ContentBlock(type=ContentType.TEXT, text="streaming chunk")],
    )
    event.metadata = {"_token_stream": True, "_inbound_event_id": "ev1"}
    event.is_final = False
    event.message_kind = "streaming"

    await manager._filter_and_dispatch(event)
    # Token stream events should be handled without error
    # (actual delivery depends on stream state, but no exception = success)
