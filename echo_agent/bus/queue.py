"""Async message bus with pub/sub for event routing."""

from __future__ import annotations

import asyncio
from collections import defaultdict
from typing import Awaitable, Callable

from loguru import logger

from echo_agent.bus.events import InboundEvent, OutboundEvent

InboundHandler = Callable[[InboundEvent], Awaitable[None]]
OutboundHandler = Callable[[OutboundEvent], Awaitable[None]]


class MessageBus:
    """Central event bus that decouples channels from the agent loop.

    Channels publish inbound events; the agent loop subscribes.
    The agent loop publishes outbound events; channels subscribe.
    """

    def __init__(self, max_queue_size: int = 1000):
        self._inbound_queue: asyncio.Queue[InboundEvent] = asyncio.Queue(maxsize=max_queue_size)
        self._outbound_handlers: dict[str, list[OutboundHandler]] = defaultdict(list)
        self._global_outbound_handlers: list[OutboundHandler] = []
        self._inbound_subscribers: list[InboundHandler] = []
        self._running = False
        self._dispatch_task: asyncio.Task | None = None

    async def publish_inbound(self, event: InboundEvent) -> None:
        try:
            self._inbound_queue.put_nowait(event)
        except asyncio.QueueFull:
            logger.warning("Inbound queue full, dropping event from {}:{}", event.channel, event.chat_id)

    async def publish_outbound(self, event: OutboundEvent) -> None:
        handlers = list(self._global_outbound_handlers)
        if event.channel in self._outbound_handlers:
            handlers.extend(self._outbound_handlers[event.channel])

        if not handlers:
            logger.warning("No outbound handler for channel={}", event.channel)
            return

        global_results = await asyncio.gather(
            *(handler(event) for handler in self._global_outbound_handlers),
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

    def subscribe_outbound(self, channel: str, handler: OutboundHandler) -> None:
        self._outbound_handlers[channel].append(handler)

    def subscribe_outbound_global(self, handler: OutboundHandler) -> None:
        self._global_outbound_handlers.append(handler)

    async def start(self) -> None:
        self._running = True
        self._dispatch_task = asyncio.create_task(self._dispatch_loop())
        logger.info("MessageBus started")

    async def stop(self) -> None:
        self._running = False
        if self._dispatch_task:
            self._dispatch_task.cancel()
            try:
                await self._dispatch_task
            except asyncio.CancelledError:
                pass
        logger.info("MessageBus stopped")

    async def _dispatch_loop(self) -> None:
        while self._running:
            try:
                event = await asyncio.wait_for(self._inbound_queue.get(), timeout=1.0)
            except asyncio.TimeoutError:
                continue
            except asyncio.CancelledError:
                break

            for handler in self._inbound_subscribers:
                try:
                    await handler(event)
                except Exception as e:
                    logger.error("Inbound handler failed for event {}: {}", event.event_id, e)

    @property
    def pending_inbound(self) -> int:
        return self._inbound_queue.qsize()
