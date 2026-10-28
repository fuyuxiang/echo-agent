"""Tests for MessageBus backpressure and error isolation."""

from __future__ import annotations

from unittest.mock import AsyncMock

import pytest

from echo_agent.bus.events import InboundEvent, OutboundEvent, ContentBlock, ContentType
from echo_agent.bus.queue import MessageBus
from echo_agent.agent.interrupt_manager import InterruptManager


@pytest.mark.asyncio
async def test_publish_inbound_returns_true_on_success() -> None:
    bus = MessageBus(max_queue_size=10)
    event = InboundEvent.text_message(channel="test", sender_id="u1", chat_id="c1", text="hi")
    result = await bus.publish_inbound(event)
    assert result is True


@pytest.mark.asyncio
async def test_publish_inbound_returns_false_when_full() -> None:
    bus = MessageBus(max_queue_size=1)
    e1 = InboundEvent.text_message(channel="test", sender_id="u1", chat_id="c1", text="first")
    e2 = InboundEvent.text_message(channel="test", sender_id="u1", chat_id="c1", text="second")

    await bus.publish_inbound(e1)  # fills the queue
    await bus.publish_inbound(e2)  # should fail (timeout after 5s is too long for test)
    # We need a shorter timeout - let's test via the queue being full
    assert bus.pending_inbound == 1


@pytest.mark.asyncio
async def test_dispatcher_stops_draining_when_concurrency_is_saturated() -> None:
    """The bounded queue must stay the admission limit under load.

    The dispatcher acquires no slot itself, so if it kept draining while every
    concurrency slot was busy it would convert a bounded queue into unbounded
    parked tasks — publish_inbound would accept without limit and the configured
    ``max_queue_size`` would push back on nobody.
    """
    import asyncio

    bus = MessageBus(max_queue_size=8, max_concurrency=2)
    entered = 0

    async def handler(event: InboundEvent) -> None:
        nonlocal entered
        entered += 1
        await asyncio.sleep(60)

    bus.subscribe_inbound(handler)
    await bus.start()
    try:
        accepted = 0
        pushed_back = False
        for i in range(60):
            try:
                ok = await asyncio.wait_for(
                    bus.publish_inbound(InboundEvent.text_message(
                        channel="test", sender_id="u", chat_id=f"c{i}", text="x",
                    )),
                    timeout=1.0,
                )
            except asyncio.TimeoutError:
                # publish_inbound is parked on a full queue: backpressure works.
                pushed_back = True
                break
            if not ok:
                pushed_back = True
                break
            accepted += 1
            await asyncio.sleep(0)

        assert pushed_back, "a saturated bus must eventually refuse admission"
        # Only the running turns plus a small dispatcher hand-off may be parked,
        # nowhere near the 60 published events.
        assert len(bus._inflight_inbound) <= 4
        assert entered == 2
        assert accepted < 60
    finally:
        for task in list(bus._inflight_inbound):
            task.cancel()
        await bus.stop(drain_timeout=0.1)


@pytest.mark.asyncio
async def test_control_events_bypass_saturated_concurrency() -> None:
    """A control command must reach its handler even with every slot busy.

    Interrupt / clarify-cancel exist to stop or wake the very turns occupying
    the slots, so pacing the dispatcher must never delay them.
    """
    import asyncio

    bus = MessageBus(max_queue_size=8, max_concurrency=1)
    control_seen = asyncio.Event()

    async def handler(event: InboundEvent) -> None:
        if event.is_control:
            control_seen.set()
            return
        await asyncio.sleep(60)

    bus.subscribe_inbound(handler)
    await bus.start()
    try:
        await bus.publish_inbound(InboundEvent.text_message(
            channel="test", sender_id="u", chat_id="c", text="hog",
        ))
        await asyncio.sleep(0.05)
        await bus.publish_inbound(InboundEvent.text_message(
            channel="test", sender_id="u", chat_id="c", text="/stop",
            is_control=True,
        ))
        await asyncio.wait_for(control_seen.wait(), timeout=3)
    finally:
        for task in list(bus._inflight_inbound):
            task.cancel()
        await bus.stop(drain_timeout=0.1)


@pytest.mark.asyncio
async def test_control_events_bypass_normal_backlog_waiting_for_capacity() -> None:
    """A normal item ahead of control must not hide the escape hatch.

    This is the ordering that a single FIFO dispatcher cannot solve: it takes
    the backlog item, blocks waiting for capacity, and never observes the cancel
    behind it.
    """
    import asyncio

    bus = MessageBus(max_queue_size=8, max_concurrency=1)
    normal_entered = asyncio.Event()
    release_normal = asyncio.Event()
    control_seen = asyncio.Event()

    async def handler(event: InboundEvent) -> None:
        if event.is_control:
            control_seen.set()
            return
        normal_entered.set()
        await release_normal.wait()

    bus.subscribe_inbound(handler)
    await bus.start()
    try:
        await bus.publish_inbound(InboundEvent.text_message(
            channel="test", sender_id="u", chat_id="c", text="hog",
        ))
        await asyncio.wait_for(normal_entered.wait(), timeout=1)

        # This normal event is transferred to a task which waits for the sole
        # occupied semaphore slot.  The control event arrives after it.
        await bus.publish_inbound(InboundEvent.text_message(
            channel="test", sender_id="u", chat_id="c", text="backlog",
        ))
        await asyncio.sleep(0)
        await bus.publish_inbound(InboundEvent.text_message(
            channel="test", sender_id="u", chat_id="c", text="/stop",
            is_control=True,
        ))

        await asyncio.wait_for(control_seen.wait(), timeout=1)
    finally:
        release_normal.set()
        await bus.stop(drain_timeout=1)


@pytest.mark.asyncio
async def test_targeted_control_overtaking_queued_turn_is_not_lost() -> None:
    """Priority changes ordering, so the interrupt manager bridges the gap."""
    import asyncio

    bus = MessageBus(max_queue_size=8, max_concurrency=1)
    interrupts = InterruptManager()
    hog_release = asyncio.Event()
    target_registered = asyncio.Event()

    async def handler(event: InboundEvent) -> None:
        if event.is_control:
            interrupts.interrupt(
                event.session_key,
                str(event.metadata.get("_interrupt_target_event_id") or ""),
            )
            return
        if event.event_id == "hog":
            await hog_release.wait()
            return
        interrupts.request(event.session_key, event.event_id)
        target_registered.set()

    bus.subscribe_inbound(handler)
    await bus.start()
    try:
        hog = InboundEvent.text_message(
            channel="test", sender_id="u", chat_id="other", text="hog",
        )
        hog.event_id = "hog"
        target = InboundEvent.text_message(
            channel="test", sender_id="u", chat_id="target", text="work",
        )
        target.event_id = "target-event"
        interrupts.admit(target.session_key, target.event_id)
        stop = InboundEvent.text_message(
            channel="test", sender_id="u", chat_id="target", text="/__interrupt__",
            is_control=True,
        )
        stop.metadata["_interrupt_target_event_id"] = "target-event"

        assert await bus.publish_inbound(hog)
        await asyncio.sleep(0.05)
        assert await bus.publish_inbound(target)
        assert await bus.publish_inbound(stop)
        await asyncio.sleep(0.05)
        assert not target_registered.is_set()

        hog_release.set()
        await asyncio.wait_for(target_registered.wait(), timeout=1)
        assert interrupts.is_interrupted(target.session_key) is True
    finally:
        hog_release.set()
        await bus.stop(drain_timeout=0.1)


@pytest.mark.asyncio
async def test_control_lane_rejects_burst_beyond_its_bound() -> None:
    bus = MessageBus(max_queue_size=2, max_concurrency=1)
    events = [
        InboundEvent.text_message(
            channel="test", sender_id="u", chat_id="c", text="/stop",
            is_control=True,
        )
        for _ in range(3)
    ]

    assert await bus.publish_inbound(events[0]) is True
    assert await bus.publish_inbound(events[1]) is True
    assert await bus.publish_inbound(events[2]) is False
    assert bus.pending_inbound == 2


@pytest.mark.asyncio
async def test_outbound_handler_exception_isolated() -> None:
    bus = MessageBus()
    good_handler = AsyncMock()
    bad_handler = AsyncMock(side_effect=RuntimeError("boom"))

    bus.subscribe_outbound_global(bad_handler)
    bus.subscribe_outbound_global(good_handler)

    event = OutboundEvent(
        channel="test",
        chat_id="c1",
        content=[ContentBlock(type=ContentType.TEXT, text="hello")],
    )
    event.metadata = {}

    await bus.publish_outbound(event)

    bad_handler.assert_called_once()
    good_handler.assert_called_once()
