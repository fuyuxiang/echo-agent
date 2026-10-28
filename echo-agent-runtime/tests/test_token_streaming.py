from __future__ import annotations

from typing import Any

import pytest

from echo_agent.agent.loop import _TokenStreamPublisher
from echo_agent.bus.events import InboundEvent, OutboundEvent
from echo_agent.bus.queue import MessageBus
from echo_agent.channels.base import BaseChannel, SendResult
from echo_agent.channels.manager import ChannelManager
from echo_agent.config.schema import ChannelsConfig


class _FakeChannel(BaseChannel):
    def __init__(self, name: str, bus: MessageBus, *, supports_edit: bool):
        super().__init__(config=object(), bus=bus)
        self.name = name
        self.supports_edit = supports_edit
        self._running = True
        self.sent: list[OutboundEvent] = []
        self.edits: list[tuple[str, str, str]] = []
        self._next_id = 0

    async def start(self) -> None:
        self._running = True

    async def stop(self) -> None:
        self._running = False

    async def send(self, event: OutboundEvent) -> SendResult:
        self._next_id += 1
        self.sent.append(event)
        return SendResult(success=True, message_id=f"msg-{self._next_id}")

    async def edit_message(
        self,
        chat_id: str,
        message_id: str,
        text: str,
        *,
        metadata: dict[str, Any] | None = None,
        finalize: bool = False,
    ) -> SendResult:
        self.edits.append((chat_id, message_id, text))
        return SendResult(success=True, message_id=message_id)


def _stream_event(text: str, *, channel: str, final: bool, full_text: bool = False) -> OutboundEvent:
    event = OutboundEvent.text_reply(channel=channel, chat_id="chat-1", text=text, reply_to_id="reply-1")
    event.is_final = final
    event.message_kind = "final" if final else "streaming"
    event.metadata = {
        "_token_stream": True,
        "_inbound_event_id": "inbound-1",
    }
    if full_text:
        event.metadata["_stream_full_text"] = True
    return event


@pytest.mark.asyncio
async def test_token_stream_non_edit_channel_only_delivers_final_text() -> None:
    bus = MessageBus()
    manager = ChannelManager(ChannelsConfig(), bus)
    channel = _FakeChannel("weixin", bus, supports_edit=False)
    manager._channels["weixin"] = channel
    bus.subscribe_outbound("weixin", channel.send)

    await bus.publish_outbound(_stream_event("hello", channel="weixin", final=False))
    await bus.publish_outbound(_stream_event("hello world", channel="weixin", final=True, full_text=True))

    assert [event.text for event in channel.sent] == ["hello world"]
    assert channel.edits == []


@pytest.mark.asyncio
async def test_token_stream_edit_channel_sends_once_then_edits_in_place() -> None:
    bus = MessageBus()
    manager = ChannelManager(ChannelsConfig(), bus)
    channel = _FakeChannel("telegram", bus, supports_edit=True)
    manager._channels["telegram"] = channel
    bus.subscribe_outbound("telegram", channel.send)

    await bus.publish_outbound(_stream_event("hello", channel="telegram", final=False))
    await bus.publish_outbound(_stream_event(" world", channel="telegram", final=False))
    await bus.publish_outbound(_stream_event("hello world", channel="telegram", final=True, full_text=True))

    assert [event.text for event in channel.sent] == ["hello ..."]
    assert channel.edits == [
        ("chat-1", "msg-1", "hello world ..."),
        ("chat-1", "msg-1", "hello world"),
    ]


@pytest.mark.asyncio
async def test_token_stream_edit_channel_hides_thinking_blocks() -> None:
    bus = MessageBus()
    manager = ChannelManager(ChannelsConfig(), bus)
    channel = _FakeChannel("telegram", bus, supports_edit=True)
    manager._channels["telegram"] = channel
    bus.subscribe_outbound("telegram", channel.send)

    await bus.publish_outbound(_stream_event("<think>secret", channel="telegram", final=False))
    await bus.publish_outbound(_stream_event("</think>visible", channel="telegram", final=False))
    await bus.publish_outbound(_stream_event("visible", channel="telegram", final=True, full_text=True))

    assert [event.text for event in channel.sent] == ["visible ..."]
    assert channel.edits == [("chat-1", "msg-1", "visible")]


@pytest.mark.asyncio
async def test_token_stream_publisher_final_event_contains_full_text_after_chunks() -> None:
    bus = MessageBus()
    captured: list[OutboundEvent] = []

    async def capture(event: OutboundEvent) -> None:
        captured.append(event)

    bus.subscribe_outbound_global(capture)
    inbound = InboundEvent.text_message(channel="telegram", sender_id="user-1", chat_id="chat-1", text="hi")
    publisher = _TokenStreamPublisher(
        bus,
        inbound,
        enabled=True,
        flush_chars=2,
        flush_interval_ms=1000,
        paragraph_mode=False,
    )

    await publisher.on_delta("he")
    await publisher.on_delta("llo")
    await publisher.finalize("hello")

    assert [(event.text, event.is_final) for event in captured] == [
        ("he", False),
        ("llo", False),
        ("hello", True),
    ]
    assert captured[-1].metadata["_stream_full_text"] is True


def _final_event(text: str, *, channel: str) -> OutboundEvent:
    event = OutboundEvent.text_reply(channel=channel, chat_id="chat-1", text=text, reply_to_id="reply-1")
    event.is_final = True
    event.message_kind = "final"
    event.metadata = {"_inbound_event_id": "inbound-1"}
    return event


@pytest.mark.asyncio
async def test_non_stream_final_event_delivered_via_deliver_final() -> None:
    bus = MessageBus()
    manager = ChannelManager(ChannelsConfig(), bus)
    channel = _FakeChannel("qqbot", bus, supports_edit=False)
    manager._channels["qqbot"] = channel
    bus.subscribe_outbound("qqbot", channel.send)

    event = _final_event("hello from agent", channel="qqbot")
    await bus.publish_outbound(event)

    assert len(channel.sent) == 1
    assert channel.sent[0].text == "hello from agent"
    assert event.metadata.get("_drop") is True


@pytest.mark.asyncio
async def test_non_stream_final_event_empty_text_not_delivered() -> None:
    bus = MessageBus()
    manager = ChannelManager(ChannelsConfig(), bus)
    channel = _FakeChannel("qqbot", bus, supports_edit=False)
    manager._channels["qqbot"] = channel
    bus.subscribe_outbound("qqbot", channel.send)

    event = _final_event("", channel="qqbot")
    await bus.publish_outbound(event)

    assert len(channel.sent) == 0


@pytest.mark.asyncio
async def test_non_stream_final_event_no_double_delivery() -> None:
    """_drop prevents bus channel handler from sending again."""
    bus = MessageBus()
    manager = ChannelManager(ChannelsConfig(), bus)
    channel = _FakeChannel("qqbot", bus, supports_edit=False)
    manager._channels["qqbot"] = channel
    bus.subscribe_outbound("qqbot", channel.send)

    event = _final_event("single delivery", channel="qqbot")
    await bus.publish_outbound(event)

    assert len(channel.sent) == 1


@pytest.mark.asyncio
async def test_stream_channel_still_works_after_refactor() -> None:
    """Streaming channels are unaffected by the _deliver_final addition."""
    bus = MessageBus()
    manager = ChannelManager(ChannelsConfig(), bus)
    channel = _FakeChannel("telegram", bus, supports_edit=True)
    manager._channels["telegram"] = channel
    bus.subscribe_outbound("telegram", channel.send)

    await bus.publish_outbound(_stream_event("hello", channel="telegram", final=False))
    await bus.publish_outbound(_stream_event("hello world", channel="telegram", final=True, full_text=True))

    assert [event.text for event in channel.sent] == ["hello ..."]
    assert channel.edits[-1] == ("chat-1", "msg-1", "hello world")
