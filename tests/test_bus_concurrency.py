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
