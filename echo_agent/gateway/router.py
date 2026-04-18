"""Delivery router — cross-platform and cron output routing."""

from __future__ import annotations

from typing import Any, Callable

from loguru import logger

from echo_agent.bus.events import OutboundEvent, ContentBlock, ContentType
from echo_agent.bus.queue import MessageBus


class DeliveryRouter:

    def __init__(self, bus: MessageBus):
        self._bus = bus
        self._rules: list[tuple[Callable[[OutboundEvent], bool], str, str]] = []
        bus.subscribe_outbound_global(self._on_outbound)

    def add_rule(
        self,
        predicate: Callable[[OutboundEvent], bool],
        target_channel: str,
        target_chat_id: str,
    ) -> None:
        self._rules.append((predicate, target_channel, target_chat_id))

    def add_cron_route(
        self,
        cron_session_key: str,
        target_channel: str,
        target_chat_id: str,
    ) -> None:
        def _match(event: OutboundEvent) -> bool:
            source = event.metadata.get("source_session_key", "")
            return source == cron_session_key or (
                source.startswith("cron:") and cron_session_key == "*"
            )

        self.add_rule(_match, target_channel, target_chat_id)
        logger.info(
            "Cron route: {} → {}:{}",
            cron_session_key, target_channel, target_chat_id,
        )

    def add_mirror_route(
        self,
        source_channel: str,
        target_channel: str,
        target_chat_id: str,
    ) -> None:
        def _match(event: OutboundEvent) -> bool:
            return event.channel == source_channel and not event.metadata.get("_routed")

        self.add_rule(_match, target_channel, target_chat_id)
        logger.info(
            "Mirror route: {} → {}:{}",
            source_channel, target_channel, target_chat_id,
        )

    async def _on_outbound(self, event: OutboundEvent) -> None:
        if event.metadata.get("_routed"):
            return

        for predicate, target_channel, target_chat_id in self._rules:
            try:
                if not predicate(event):
                    continue
            except Exception:
                continue

            routed = OutboundEvent(
                channel=target_channel,
                chat_id=target_chat_id,
                content=list(event.content),
                reply_to_id=event.reply_to_id,
                metadata={**event.metadata, "_routed": True},
                is_final=event.is_final,
                message_kind=event.message_kind,
            )
            await self._bus.publish_outbound(routed)

    @property
    def rule_count(self) -> int:
        return len(self._rules)
