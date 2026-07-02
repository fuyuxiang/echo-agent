"""Cognitive event emitter — fans AgentLoop pipeline signals out to an
attached cli TUI. Gated to cli sessions only (spec decision 9); IM channels
skip on the first line at zero cost."""

from __future__ import annotations

import uuid

from echo_agent.bus.events import InboundEvent, OutboundEvent
from echo_agent.bus.queue import MessageBus

CLI_CHANNEL = "gateway:cli"


def should_emit_cognitive(channel: str) -> bool:
    """Only the cli session renders cognitive frames. The single extension
    point if a future web console needs them (spec decision 9)."""
    return channel == CLI_CHANNEL


class CognitiveEmitter:
    def __init__(self, bus: MessageBus) -> None:
        self._bus = bus

    async def emit(
        self, event: InboundEvent, cog_type: str, data: dict, summary: str
    ) -> None:
        if not should_emit_cognitive(event.channel):
            return
        out = OutboundEvent.text_reply(
            channel=event.channel, chat_id=event.chat_id, text=summary,
            reply_to_id=event.reply_to_id,
        )
        out.is_final = False
        out.message_kind = "cognitive"
        out.metadata = {
            "_progress": True,
            "cog_type": cog_type,
            "cog_event_id": "evt_" + uuid.uuid4().hex[:12],
            "_inbound_event_id": event.event_id,
            "data": data,
        }
        await self._bus.publish_outbound(out)
