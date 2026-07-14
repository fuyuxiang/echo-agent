"""Tests for MessageBus concurrency control and rate limiting."""

import asyncio

import pytest

from echo_agent.bus.events import InboundEvent, OutboundEvent
from echo_agent.bus.queue import MessageBus
from echo_agent.bus.rate_limiter import SessionRateLimiter


class TestBusConcurrency:
    @pytest.mark.asyncio
    async def test_semaphore_limits_concurrent_dispatch(self):
        bus = MessageBus(max_queue_size=100, max_concurrency=2)
        concurrent_count = 0
        max_concurrent = 0

        async def slow_handler(event: InboundEvent) -> None:
            nonlocal concurrent_count, max_concurrent
            concurrent_count += 1
            max_concurrent = max(max_concurrent, concurrent_count)
            await asyncio.sleep(0.05)
            concurrent_count -= 1

        bus.subscribe_inbound(slow_handler)
        await bus.start()

        for i in range(6):
            event = InboundEvent.text_message(
                channel="test", sender_id="u1", chat_id=f"chat_{i}", text="hi"
            )
            await bus.publish_inbound(event)

        await asyncio.sleep(0.3)
        await bus.stop()

        assert max_concurrent <= 2

    @pytest.mark.asyncio
    async def test_rate_limiter_rejects_excess(self):
        bus = MessageBus(max_queue_size=100, max_concurrency=50)
        limiter = SessionRateLimiter(rpm=60, burst=2)
        bus.set_rate_limiter(limiter)

        received = []
        rejected_replies = []

        async def handler(event: InboundEvent) -> None:
            received.append(event)

        async def outbound_handler(event: OutboundEvent) -> None:
            if "频繁" in event.text:
                rejected_replies.append(event)

        bus.subscribe_inbound(handler)
        bus.subscribe_outbound("test", outbound_handler)
        await bus.start()

        for i in range(5):
            event = InboundEvent.text_message(
                channel="test", sender_id="u1", chat_id="same_chat", text=f"msg{i}"
            )
            await bus.publish_inbound(event)

        await asyncio.sleep(0.3)
        await bus.stop()

        assert len(received) == 2
        assert len(rejected_replies) == 3

    @pytest.mark.asyncio
    async def test_control_event_bypasses_rate_limiter(self):
        # A control event (e.g. the clarify-cancel escape valve synthesized on
        # ws disconnect) must reach the handler even when the session's rate
        # bucket is fully drained — otherwise a user who just flooded the
        # session leaves the agent parked until the 24h backstop.
        bus = MessageBus(max_queue_size=100, max_concurrency=50)
        limiter = SessionRateLimiter(rpm=60, burst=2)
        bus.set_rate_limiter(limiter)

        received = []

        async def handler(event: InboundEvent) -> None:
            received.append(event)

        bus.subscribe_inbound(handler)
        await bus.start()

        # Drain the burst so any normal message would now be rejected.
        for i in range(3):
            await bus.publish_inbound(InboundEvent.text_message(
                channel="test", sender_id="u1", chat_id="same_chat", text=f"msg{i}"
            ))
        # The control event must still get through.
        await bus.publish_inbound(InboundEvent.text_message(
            channel="test", sender_id="u1", chat_id="same_chat",
            text="/__clarify_cancel__", is_control=True,
        ))

        await asyncio.sleep(0.3)
        await bus.stop()

        assert any(e.is_control for e in received)
        assert received[-1].text == "/__clarify_cancel__"

    @pytest.mark.asyncio
    async def test_no_rate_limiter_allows_all(self):
        bus = MessageBus(max_queue_size=100, max_concurrency=50)

        received = []

        async def handler(event: InboundEvent) -> None:
            received.append(event)

        bus.subscribe_inbound(handler)
        await bus.start()

        for i in range(5):
            event = InboundEvent.text_message(
                channel="test", sender_id="u1", chat_id="chat", text=f"msg{i}"
            )
            await bus.publish_inbound(event)

        await asyncio.sleep(0.2)
        await bus.stop()

        assert len(received) == 5

    @pytest.mark.asyncio
    async def test_guarded_dispatch_releases_semaphore_on_error(self):
        bus = MessageBus(max_queue_size=10, max_concurrency=2)

        call_count = 0

        async def failing_handler(event: InboundEvent) -> None:
            nonlocal call_count
            call_count += 1
            raise RuntimeError("handler error")

        bus.subscribe_inbound(failing_handler)
        await bus.start()

        for i in range(4):
            event = InboundEvent.text_message(
                channel="test", sender_id="u1", chat_id=f"c{i}", text="hi"
            )
            await bus.publish_inbound(event)

        await asyncio.sleep(0.2)
        await bus.stop()

        assert call_count == 4


class TestBusUnsubscribe:
    @pytest.mark.asyncio
    async def test_unsubscribe_inbound_stops_delivery(self):
        bus = MessageBus(max_queue_size=100, max_concurrency=50)
        received = []

        async def handler(event: InboundEvent) -> None:
            received.append(event)

        bus.subscribe_inbound(handler)
        await bus.start()

        event1 = InboundEvent.text_message(channel="test", sender_id="u1", chat_id="c1", text="first")
        await bus.publish_inbound(event1)
        await asyncio.sleep(0.1)

        bus.unsubscribe_inbound(handler)

        event2 = InboundEvent.text_message(channel="test", sender_id="u1", chat_id="c2", text="second")
        await bus.publish_inbound(event2)
        await asyncio.sleep(0.1)
        await bus.stop()

        assert len(received) == 1
        assert received[0].text == "first"

    @pytest.mark.asyncio
    async def test_unsubscribe_outbound_stops_delivery(self):
        bus = MessageBus(max_queue_size=100, max_concurrency=50)
        received = []

        async def handler(event: OutboundEvent) -> None:
            received.append(event)

        bus.subscribe_outbound("test", handler)
        await bus.start()

        out1 = OutboundEvent.text_reply(channel="test", chat_id="c1", text="first")
        await bus.publish_outbound(out1)

        bus.unsubscribe_outbound("test", handler)

        out2 = OutboundEvent.text_reply(channel="test", chat_id="c2", text="second")
        await bus.publish_outbound(out2)
        await bus.stop()

        assert len(received) == 1

    @pytest.mark.asyncio
    async def test_unsubscribe_nonexistent_is_noop(self):
        bus = MessageBus()

        async def handler(event: InboundEvent) -> None:
            pass

        bus.unsubscribe_inbound(handler)
        bus.unsubscribe_outbound("fake", handler)
        bus.unsubscribe_outbound_global(handler)
