"""Async message bus with pub/sub for event routing."""

from __future__ import annotations

import asyncio
from collections import defaultdict
from typing import Awaitable, Callable

from loguru import logger

from echo_agent.bus.events import InboundEvent, OutboundEvent
from echo_agent.bus.rate_limiter import SessionRateLimiter

InboundHandler = Callable[[InboundEvent], Awaitable[None]]
OutboundHandler = Callable[[OutboundEvent], Awaitable[None]]


class MessageBus:
    """Central event bus that decouples channels from the agent loop.

    Channels publish inbound events; the agent loop subscribes.
    The agent loop publishes outbound events; channels subscribe.
    """

    def __init__(self, max_queue_size: int = 1000, max_concurrency: int = 50):
        self._inbound_queue: asyncio.Queue[InboundEvent] = asyncio.Queue(maxsize=max_queue_size)
        self._outbound_handlers: dict[str, list[OutboundHandler]] = defaultdict(list)
        self._global_outbound_handlers: list[OutboundHandler] = []
        self._inbound_subscribers: list[InboundHandler] = []
        self._running = False
        self._accepting = True
        self._dispatch_task: asyncio.Task | None = None
        self._inflight_inbound: set[asyncio.Task] = set()
        self._lifecycle_lock = asyncio.Lock()
        self._concurrency_sem = asyncio.Semaphore(max_concurrency)
        self._rate_limiter: SessionRateLimiter | None = None

    def set_rate_limiter(self, limiter: SessionRateLimiter) -> None:
        self._rate_limiter = limiter

    async def publish_inbound(self, event: InboundEvent) -> bool:
        if not self._accepting:
            logger.warning("Bus is shutting down, rejecting event from {}:{}", event.channel, event.chat_id)
            return False
        try:
            await asyncio.wait_for(self._inbound_queue.put(event), timeout=5.0)
            return True
        except asyncio.TimeoutError:
            logger.error("Inbound queue full after 5s wait, rejecting event from {}:{}", event.channel, event.chat_id)
            return False

    async def publish_outbound(self, event: OutboundEvent) -> None:
        if not self._global_outbound_handlers and event.channel not in self._outbound_handlers:
            logger.warning("No outbound handler for channel={}", event.channel)
            return

        # Snapshot handler lists before iterating so a handler that mutates the list
        # (e.g. subscribes/unsubscribes) won't trigger "list changed size" errors.
        global_handlers = list(self._global_outbound_handlers)
        global_results = await asyncio.gather(
            *(handler(event) for handler in global_handlers),
            return_exceptions=True,
        )
        for i, result in enumerate(global_results):
            if isinstance(result, Exception):
                logger.error("Global outbound handler {} failed: {}", i, result)

        if event.metadata.get("_drop"):
            return

        specific_handlers = list(self._outbound_handlers.get(event.channel, []))
        if not specific_handlers:
            return

        specific_results = await asyncio.gather(
            *(handler(event) for handler in specific_handlers),
            return_exceptions=True,
        )
        for i, result in enumerate(specific_results):
            if isinstance(result, Exception):
                logger.error("Outbound handler {} for channel {} failed: {}", i, event.channel, result)

    def subscribe_inbound(self, handler: InboundHandler) -> None:
        self._inbound_subscribers.append(handler)

    def unsubscribe_inbound(self, handler: InboundHandler) -> None:
        try:
            self._inbound_subscribers.remove(handler)
        except ValueError:
            pass

    def subscribe_outbound(self, channel: str, handler: OutboundHandler) -> None:
        self._outbound_handlers[channel].append(handler)

    def unsubscribe_outbound(self, channel: str, handler: OutboundHandler) -> None:
        handlers = self._outbound_handlers.get(channel)
        if handlers:
            try:
                handlers.remove(handler)
            except ValueError:
                pass
            if not handlers:
                del self._outbound_handlers[channel]

    def subscribe_outbound_global(self, handler: OutboundHandler) -> None:
        self._global_outbound_handlers.append(handler)

    def unsubscribe_outbound_global(self, handler: OutboundHandler) -> None:
        try:
            self._global_outbound_handlers.remove(handler)
        except ValueError:
            pass

    async def start(self) -> None:
        async with self._lifecycle_lock:
            if self._running:
                return
            self._running = True
            self._accepting = True
            self._dispatch_task = asyncio.create_task(self._dispatch_loop())
        logger.info("MessageBus started")

    async def stop(self, drain_timeout: float = 10.0) -> None:
        async with self._lifecycle_lock:
            self._accepting = False
            self._running = False
            if self._dispatch_task:
                self._dispatch_task.cancel()
                try:
                    await self._dispatch_task
                except asyncio.CancelledError:
                    pass
                self._dispatch_task = None
            # Give in-flight turns a chance to finish (and persist their
            # sessions) before hard-cancelling them.
            if self._inflight_inbound:
                tasks = list(self._inflight_inbound)
                done, pending = await asyncio.wait(tasks, timeout=drain_timeout)
                if pending:
                    logger.warning(
                        "{} in-flight event(s) did not finish within {}s, cancelling",
                        len(pending), drain_timeout,
                    )
                    for task in pending:
                        task.cancel()
                    await asyncio.gather(*pending, return_exceptions=True)
                self._inflight_inbound.clear()
            if not self._inbound_queue.empty():
                dropped = self._inbound_queue.qsize()
                logger.warning("Discarding {} queued inbound event(s) on shutdown", dropped)
        logger.info("MessageBus stopped")

    async def _dispatch_loop(self) -> None:
        while self._running:
            try:
                event = await asyncio.wait_for(self._inbound_queue.get(), timeout=1.0)
            except asyncio.TimeoutError:
                continue
            except asyncio.CancelledError:
                break

            if self._rate_limiter and not self._rate_limiter.try_acquire(event.session_key):
                logger.warning("Rate limited session {}", event.session_key)
                rate_limit_reply = OutboundEvent.text_reply(
                    channel=event.channel,
                    chat_id=event.chat_id,
                    text="请求过于频繁，请稍后再试。",
                    reply_to_id=event.reply_to_id,
                )
                # Send the reply off-loop: a slow outbound handler must not
                # stall inbound dispatch for every other session.
                reply_task = asyncio.create_task(self._publish_outbound_logged(rate_limit_reply))
                self._inflight_inbound.add(reply_task)
                reply_task.add_done_callback(self._inflight_inbound.discard)
                continue

            await self._concurrency_sem.acquire()
            task = asyncio.create_task(self._dispatch_inbound_guarded(event))
            self._inflight_inbound.add(task)
            task.add_done_callback(self._inflight_inbound.discard)

    async def _dispatch_inbound_guarded(self, event: InboundEvent) -> None:
        try:
            await self._dispatch_inbound_event(event)
        finally:
            self._concurrency_sem.release()

    async def _publish_outbound_logged(self, event: OutboundEvent) -> None:
        try:
            await self.publish_outbound(event)
        except Exception as e:
            logger.error("Failed to publish outbound event for {}: {}", event.channel, e)

    async def _dispatch_inbound_event(self, event: InboundEvent) -> None:
        # Snapshot subscribers list to be safe against concurrent (un)subscriptions.
        subscribers = list(self._inbound_subscribers)
        results = await asyncio.gather(
            *(handler(event) for handler in subscribers),
            return_exceptions=True,
        )
        for result in results:
            if isinstance(result, Exception):
                logger.error("Inbound handler failed for event {}: {}", event.event_id, result)

    @property
    def pending_inbound(self) -> int:
        return self._inbound_queue.qsize()
