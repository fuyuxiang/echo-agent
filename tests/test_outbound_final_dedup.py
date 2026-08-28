"""One turn must not deliver two final messages to the same target.

Observed as a cron weather job posting twice to WeChat at 06:30, with *different*
content each time — so not the known iLinkAI retry (which duplicates verbatim).
The two messages came from two different places:

  1. the `message` tool delivering the report the job asked for, and
  2. the turn's own final reply — a "✅ 已推送" wrap-up written for the caller,
     not for the user — delivered by loop.py because nothing told it the report
     had already gone out.

Three facts combined to allow it:

  - OutboundEvent defaults to is_final=True / message_kind="final", so a tool's
    own event is treated exactly like a turn's final answer;
  - the tools published without `_inbound_event_id`, so their message belonged to
    no turn and could not be related to the reply that followed;
  - `_finalized_keys` — commented as the "authoritative finalize guard" — was
    only ever written by _deliver_final and read by _handle_heartbeat. It guarded
    "a late heartbeat must not overwrite the answer" and nothing else.

The guard now also records which targets a turn has delivered to, and suppresses
the turn's own reply to a target a tool already delivered to. Suppression is
per-target and one-directional on purpose; the tests below pin both limits.
"""
from __future__ import annotations

import asyncio
from unittest.mock import MagicMock

import pytest

from echo_agent.bus.events import ContentBlock, ContentType, OutboundEvent
from echo_agent.bus.queue import MessageBus
from echo_agent.channels.base import SendResult
from echo_agent.channels.manager import ChannelManager
from echo_agent.tools import ToolExecutionContext

TURN = "evt-abc"


class _FakeChannel:
    """A non-editable channel (like weixin), recording what it was asked to send."""

    name = "weixin"
    is_realtime = True
    supports_edit = False
    is_running = True

    def __init__(self):
        self.sent: list[str] = []
        self.config = MagicMock(reactions_enabled=False)

    async def send(self, event):
        self.sent.append(event.text)
        return SendResult(success=True)

    async def send_typing(self, *a, **k):
        pass

    async def stop_typing(self, *a, **k):
        pass

    async def send_read_receipt(self, *a, **k):
        pass


def _manager(bus: MessageBus, channel: _FakeChannel) -> ChannelManager:
    """A ChannelManager with only the state the outbound path touches.

    Built via __new__: the real __init__ wants config, channel construction and a
    cleanup task, none of which this path reads.
    """
    cm = ChannelManager.__new__(ChannelManager)
    cm.bus = bus
    cm._channels = {channel.name: channel}
    cm._stream_states = {}
    cm._heartbeat_msg_ids = {}
    cm._delivered_milestone = {}
    cm._finalized_keys = {}
    cm._finalized_targets = {}
    cm._inbound_msg_ids = {}
    cm._max_inbound_ids = 1000
    cm._state_lock = asyncio.Lock()
    cm._send_progress = False
    cm._send_tool_hints = False
    bus.subscribe_outbound_global(cm._filter_and_dispatch)
    return cm


def _ctx(chat_id: str = "room1") -> ToolExecutionContext:
    return ToolExecutionContext(
        inbound_event_id=TURN, channel="weixin", chat_id=chat_id,
    )


def _tool_event(text: str, chat_id: str = "room1") -> OutboundEvent:
    return OutboundEvent.text_reply(
        channel="weixin", chat_id=chat_id, text=text,
    ).mark_tool_delivery(_ctx(chat_id))


def _final_reply(text: str, chat_id: str = "room1") -> OutboundEvent:
    """What loop.py publishes as the turn's own answer."""
    event = OutboundEvent.text_reply(channel="weixin", chat_id=chat_id, text=text)
    event.metadata["_inbound_event_id"] = TURN
    return event


@pytest.fixture
def wired():
    bus = MessageBus()
    channel = _FakeChannel()
    manager = _manager(bus, channel)
    return bus, channel, manager


@pytest.mark.asyncio
async def test_tool_delivery_suppresses_the_turns_own_reply(wired):
    """The reported bug: report + "已推送" wrap-up became two WeChat messages."""
    bus, channel, _ = wired

    await bus.publish_outbound(_tool_event("📍 临沂天气日报 | 07-30 …"))
    await bus.publish_outbound(_final_reply("✅ 临沂天气日报已推送至微信"))

    assert channel.sent == ["📍 临沂天气日报 | 07-30 …"]


@pytest.mark.asyncio
async def test_suppressed_reply_still_reports_a_successful_delivery(wired):
    """Suppression must not read as a delivery failure.

    loop.py faults the turn (and the cron run history) when publish_outbound
    comes back not-ok. A guard that reported failure here would turn every such
    turn into a red cron run and an "error" on the board.
    """
    bus, _, _ = wired

    tool_receipt = await bus.publish_outbound(_tool_event("日报正文"))
    reply_receipt = await bus.publish_outbound(_final_reply("✅ 已推送"))

    assert tool_receipt.ok is True
    assert reply_receipt.ok is True


@pytest.mark.asyncio
async def test_notifying_another_chat_does_not_swallow_the_reply(wired):
    """Per-target, not per-turn.

    A turn that notifies a different chat must still answer the chat it is in —
    a bare per-turn flag would silence the user's own answer whenever the model
    sent a notification elsewhere.
    """
    bus, channel, _ = wired

    await bus.publish_outbound(_tool_event("通知另一个群", chat_id="other-room"))
    await bus.publish_outbound(_final_reply("这是给当前会话的回答"))

    assert channel.sent == ["通知另一个群", "这是给当前会话的回答"]


@pytest.mark.asyncio
async def test_two_tool_deliveries_to_one_target_both_go_out(wired):
    """Suppression is one-directional.

    Two `message` calls to the same chat are two messages the model explicitly
    asked for. Only the turn's *own* reply yields to an earlier claim.
    """
    bus, channel, _ = wired

    await bus.publish_outbound(_tool_event("第一条"))
    await bus.publish_outbound(_tool_event("第二条"))

    assert channel.sent == ["第一条", "第二条"]


@pytest.mark.asyncio
async def test_reply_before_tool_delivery_still_delivers_both(wired):
    """Order does not invert the rule: a tool delivery is never suppressed."""
    bus, channel, _ = wired

    await bus.publish_outbound(_final_reply("先给出回答"))
    await bus.publish_outbound(_tool_event("随后工具再发一条"))

    assert channel.sent == ["先给出回答", "随后工具再发一条"]


@pytest.mark.asyncio
async def test_unattributed_events_keep_the_previous_behaviour(wired):
    """An event with no turn identity belongs to no turn, so it is never
    suppressed — a plugin or older caller publishing directly is unaffected."""
    bus, channel, _ = wired

    await bus.publish_outbound(OutboundEvent.text_reply(
        channel="weixin", chat_id="room1", text="A",
    ))
    await bus.publish_outbound(OutboundEvent.text_reply(
        channel="weixin", chat_id="room1", text="B",
    ))

    assert channel.sent == ["A", "B"]


@pytest.mark.asyncio
async def test_next_turn_is_not_suppressed_by_the_previous_one(wired):
    """The claim is scoped to one turn. The next scheduled run of the same job
    must deliver normally, or the fix would silence every run after the first."""
    bus, channel, _ = wired

    await bus.publish_outbound(_tool_event("第一轮日报"))
    await bus.publish_outbound(_final_reply("✅ 已推送"))

    second = OutboundEvent.text_reply(channel="weixin", chat_id="room1", text="第二轮回复")
    second.metadata["_inbound_event_id"] = "evt-xyz"
    await bus.publish_outbound(second)

    assert channel.sent == ["第一轮日报", "第二轮回复"]


@pytest.mark.asyncio
async def test_media_tool_delivery_also_claims_the_target(wired):
    """send_file / tts publish content blocks rather than plain text, and they
    take the same path — an audio digest followed by a text wrap-up is the same
    duplicate in a different costume."""
    bus, channel, _ = wired

    media = OutboundEvent(
        channel="weixin",
        chat_id="room1",
        content=[ContentBlock(type=ContentType.FILE, url="/tmp/a.mp3", metadata={"name": "a.mp3"})],
    ).mark_tool_delivery(_ctx())
    await bus.publish_outbound(media)
    await bus.publish_outbound(_final_reply("✅ 语音已发送"))

    assert len(channel.sent) == 1


# ── the heartbeat guard this table originally served ─────────────────────────

@pytest.mark.asyncio
async def test_finalize_still_blocks_a_late_heartbeat(wired):
    """_finalized_keys' original job must keep working: a beat that fires after
    the answer landed cannot post a stray "正在处理中…" on top of it."""
    bus, channel, manager = wired

    await bus.publish_outbound(_final_reply("答案"))
    assert channel.sent == ["答案"]

    beat = OutboundEvent.text_reply(channel="weixin", chat_id="room1", text="正在处理中…")
    beat.metadata.update({"_heartbeat": True, "_inbound_event_id": TURN})
    await bus.publish_outbound(beat)

    assert channel.sent == ["答案"]


def test_target_sets_expire_with_their_timestamps():
    """The target set must share the timestamp's lifetime.

    _finalized_targets grows one entry per turn, so evicting _finalized_keys
    without it would leak a set per turn for the life of the process. The TTL
    sweep is inlined in _ttl_cleanup_loop's `while True`, so rather than driving
    a 60s loop this asserts the structural property: the eviction of one key
    removes the other in the same block.
    """
    import inspect

    source = inspect.getsource(ChannelManager._ttl_cleanup_loop)
    assert "del self._finalized_keys[k]" in source
    # Same loop body, so the two can never diverge in lifetime.
    assert "self._finalized_targets.pop(k, None)" in source


@pytest.mark.asyncio
async def test_target_table_is_bounded(wired):
    """Unbounded growth is the other failure mode: turns whose TTL has not yet
    elapsed still must not accumulate without limit."""
    bus, _, manager = wired
    manager._max_inbound_ids = 5

    for i in range(20):
        event = OutboundEvent.text_reply(
            channel="weixin", chat_id="room1", text=f"m{i}",
        )
        event.metadata["_inbound_event_id"] = f"turn-{i}"
        await bus.publish_outbound(event)

    assert len(manager._finalized_targets) <= manager._max_inbound_ids
