"""Cron trigger channel — generates events from scheduled jobs."""

from __future__ import annotations

from loguru import logger

from echo_agent.bus.events import EventType, InboundEvent, OutboundEvent, ContentBlock, ContentType
from echo_agent.bus.queue import MessageBus
from echo_agent.channels.base import BaseChannel, SendResult
from echo_agent.config.schema import CronChannelConfig


class CronChannel(BaseChannel):
    """Pseudo-channel that injects cron-triggered events into the bus."""

    name = "cron"

    def __init__(self, config: CronChannelConfig, bus: MessageBus):
        super().__init__(config, bus)

    async def start(self) -> None:
        self._running = True
        self.bus.subscribe_outbound(self.name, self.send)
        logger.info("Cron channel ready")

    async def stop(self) -> None:
        self._running = False

    async def send(self, event: OutboundEvent) -> SendResult | None:
        logger.info("Cron output [{}]: {}", event.chat_id, event.text[:200] if event.text else "")
        return SendResult(success=True)

    async def inject(self, job_id: str, message: str, deliver_channel: str | None = None) -> None:
        """Inject a cron-triggered event into the bus."""
        event = InboundEvent(
            event_type=EventType.CRON,
            channel=self.name,
            sender_id="cron",
            chat_id=f"cron:{job_id}",
            content=[ContentBlock(type=ContentType.TEXT, text=message)],
            session_key_override=f"cron:{job_id}",
            metadata={"job_id": job_id, "deliver_channel": deliver_channel},
        )
        await self.bus.publish_inbound(event)
