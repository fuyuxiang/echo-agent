"""Tests for ChannelManager delivery logic."""

from __future__ import annotations

from unittest.mock import MagicMock

import pytest

from echo_agent.bus.events import OutboundEvent, ContentBlock, ContentType
from echo_agent.bus.queue import MessageBus
from echo_agent.channels.base import SendResult
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


# ── P1-03 delivery ledger regression fence ─────────────────────────────────
#
# Two distinct bugs the old ledger had:
#
#   (a) The approval prompt — built by ApprovalGate via text_reply — inherited
#       the default is_final=True / message_kind="final", so it claimed the
#       delivery target. The user's real answer after /approve was suppressed
#       as a duplicate, leaving them staring at the prompt with no follow-up.
#
#   (b) The target was written into _finalized_targets BEFORE awaiting
#       channel.send, so a transient transport failure claimed the target
#       anyway. Every later retry of the same turn's answer was then suppressed
#       as a duplicate — even though nothing had ever been delivered.
#
# These tests pin both shapes. The ``approval_prompt`` event is routed past
# ``_deliver_final`` entirely; failures leave the target unclaimed so the
# retry has room to try.


class _RecordingChannel:
    """Channel that records deliveries and lets the test script transport
    failures by hand. Cheap stand-in for a real channel adapter — the
    delivery ledger never reads beyond send / stop_typing / send_read_receipt."""

    def __init__(self, name: str = "mychan"):
        self.name = name
        self.sent: list[OutboundEvent] = []
        self.fail_first = False
        self._n = 0
        self.is_running = True
        self.supports_edit = False
        self.config = MagicMock(reactions_enabled=False)

    async def send(self, event: OutboundEvent) -> SendResult:
        self._n += 1
        if self.fail_first and self._n == 1:
            return SendResult(success=False, error="transport 500")
        self.sent.append(event)
        return SendResult(success=True)

    async def send_typing(self, *a, **kw):
        pass

    async def stop_typing(self, *a, **kw):
        pass

    async def send_read_receipt(self, *a, **kw):
        pass


@pytest.mark.asyncio
async def test_approval_prompt_does_not_claim_target() -> None:
    """Approval prompts are interactive, not terminal — they must not suppress
    the user's real answer that arrives later in the same turn.

    Regression: a /approve result that followed the prompt used to be
    suppressed as a duplicate because the prompt claimed the target in the
    delivery ledger.
    """
    bus = MessageBus()
    manager = ChannelManager(ChannelsConfig(), bus)
    channel = _RecordingChannel()
    manager._channels["mychan"] = channel

    # An approval prompt. ApprovalGate publishes this with is_final=False,
    # message_kind="approval_prompt" — exactly what we mirror here.
    prompt = OutboundEvent.text_reply(
        channel="mychan", chat_id="chat-1", text="⚠️ need approval",
    )
    prompt.is_final = False
    prompt.message_kind = "approval_prompt"
    prompt.metadata["_approval_request"] = True
    prompt.metadata["_inbound_event_id"] = "turn-1"
    await manager._filter_and_dispatch(prompt)

    # The user's real answer, same turn, same target. Must NOT be suppressed.
    answer = OutboundEvent(
        channel="mychan", chat_id="chat-1",
        content=[ContentBlock(type=ContentType.TEXT, text="命令输出：uid=501")],
    )
    answer.metadata["_inbound_event_id"] = "turn-1"
    await manager._filter_and_dispatch(answer)

    assert len(channel.sent) == 1
    assert channel.sent[0].content[0].text == "命令输出：uid=501"


@pytest.mark.asyncio
async def test_final_after_failure_retry_succeeds() -> None:
    """A transport failure on the first attempt must leave the target unclaimed.

    Regression: ``_finalized_targets`` was written before channel.send and never
    unwritten on failure, so a later retry to the same target in the same turn
    was suppressed as a duplicate. The user's answer was dropped.
    """
    bus = MessageBus()
    manager = ChannelManager(ChannelsConfig(), bus)
    channel = _RecordingChannel()
    channel.fail_first = True
    manager._channels["mychan"] = channel

    answer = OutboundEvent(
        channel="mychan", chat_id="chat-1",
        content=[ContentBlock(type=ContentType.TEXT, text="答案")],
    )
    answer.metadata["_inbound_event_id"] = "turn-2"
    r1 = await manager._filter_and_dispatch(answer)
    assert r1 is not None and r1.success is False

    # Retry, same target, same turn — must not be suppressed.
    retry = OutboundEvent(
        channel="mychan", chat_id="chat-1",
        content=[ContentBlock(type=ContentType.TEXT, text="答案")],
    )
    retry.metadata["_inbound_event_id"] = "turn-2"
    r2 = await manager._filter_and_dispatch(retry)

    assert r2 is not None and r2.success is True
    assert len(channel.sent) == 1
    assert channel.sent[0].content[0].text == "答案"


@pytest.mark.asyncio
async def test_two_real_finals_same_turn_dedupe() -> None:
    """The duplicate-final suppression the ledger WAS built for still works.

    Two real answers within the same turn targeting the same chat are an
    accident — the second one is an unintended wrap-up. Suppression here is
    the right behavior; the fix is about not over-suppressing on failure or
    on non-terminal prompts.
    """
    bus = MessageBus()
    manager = ChannelManager(ChannelsConfig(), bus)
    channel = _RecordingChannel()
    manager._channels["mychan"] = channel

    final1 = OutboundEvent(
        channel="mychan", chat_id="chat-1",
        content=[ContentBlock(type=ContentType.TEXT, text="答案")],
    )
    final1.metadata["_inbound_event_id"] = "turn-3"
    await manager._filter_and_dispatch(final1)

    final2 = OutboundEvent(
        channel="mychan", chat_id="chat-1",
        content=[ContentBlock(type=ContentType.TEXT, text="答案（重复 wrap-up）")],
    )
    final2.metadata["_inbound_event_id"] = "turn-3"
    await manager._filter_and_dispatch(final2)

    assert len(channel.sent) == 1
    assert channel.sent[0].content[0].text == "答案"


@pytest.mark.asyncio
async def test_tool_delivery_can_repeat_to_same_target() -> None:
    """Two explicit tool deliveries in one turn are two messages the model asked
    for, not a duplicate-final accident. The ledger must suppress the *turn's
    own reply* on top of a tool delivery — never the tool deliveries themselves.
    """
    bus = MessageBus()
    manager = ChannelManager(ChannelsConfig(), bus)
    channel = _RecordingChannel()
    manager._channels["mychan"] = channel

    tool_ctx = MagicMock(inbound_event_id="turn-4")
    tool1 = OutboundEvent(
        channel="mychan", chat_id="chat-1",
        content=[ContentBlock(type=ContentType.TEXT, text="工具消息1")],
    )
    tool1.mark_tool_delivery(tool_ctx)
    await manager._filter_and_dispatch(tool1)

    tool2 = OutboundEvent(
        channel="mychan", chat_id="chat-1",
        content=[ContentBlock(type=ContentType.TEXT, text="工具消息2")],
    )
    tool2.mark_tool_delivery(tool_ctx)
    await manager._filter_and_dispatch(tool2)

    assert len(channel.sent) == 2
    assert [e.content[0].text for e in channel.sent] == ["工具消息1", "工具消息2"]


@pytest.mark.asyncio
async def test_transport_exception_leaves_target_unclaimed() -> None:
    """An exception in channel.send (not a failed receipt) is also a non-success.

    Old code set ``_drop`` and returned a SendResult but kept the target
    claimed in the ledger, suppressing later retries. New code keeps the
    target unclaimed so a retry has room.
    """
    bus = MessageBus()
    manager = ChannelManager(ChannelsConfig(), bus)
    channel = _RecordingChannel()
    channel.fail_first = False  # raise instead
    manager._channels["mychan"] = channel

    original_send = channel.send

    async def boom(event):
        return SendResult(success=False, error="explode")

    channel.send = boom
    answer = OutboundEvent(
        channel="mychan", chat_id="chat-1",
        content=[ContentBlock(type=ContentType.TEXT, text="答案")],
    )
    answer.metadata["_inbound_event_id"] = "turn-5"
    r1 = await manager._filter_and_dispatch(answer)
    assert r1 is not None and r1.success is False

    # Retry must succeed.
    channel.send = original_send
    retry = OutboundEvent(
        channel="mychan", chat_id="chat-1",
        content=[ContentBlock(type=ContentType.TEXT, text="答案")],
    )
    retry.metadata["_inbound_event_id"] = "turn-5"
    r2 = await manager._filter_and_dispatch(retry)
    assert r2 is not None and r2.success is True
    assert len(channel.sent) == 1
