"""Progressive message editing — debounced edit-in-place for streaming responses."""

from __future__ import annotations

import asyncio
import time
from typing import Any

from loguru import logger

from echo_agent.bus.events import OutboundEvent, ContentBlock, ContentType
from echo_agent.bus.queue import MessageBus

_MIN_EDIT_INTERVAL = 0.5


class ProgressiveEditor:

    def __init__(self, bus: MessageBus):
        self._bus = bus
        self._last_edit: dict[str, float] = {}
        self._pending: dict[str, str] = {}
        self._flush_tasks: dict[str, asyncio.Task] = {}

    async def start_edit(
        self,
        channel: str,
        chat_id: str,
        initial_text: str,
    ) -> str:
        event = OutboundEvent.text_reply(
            channel=channel,
            chat_id=chat_id,
            text=initial_text,
        )
        event.is_final = False
        event.message_kind = "streaming"
        await self._bus.publish_outbound(event)
        key = f"{channel}:{chat_id}:{event.event_id}"
        self._last_edit[key] = time.time()
        return event.event_id

    async def update(
        self,
        channel: str,
        chat_id: str,
        message_id: str,
        text: str,
    ) -> None:
        key = f"{channel}:{chat_id}:{message_id}"
        now = time.time()
        last = self._last_edit.get(key, 0)

        if now - last < _MIN_EDIT_INTERVAL:
            self._pending[key] = text
            if key not in self._flush_tasks or self._flush_tasks[key].done():
                self._flush_tasks[key] = asyncio.create_task(
                    self._delayed_flush(channel, chat_id, message_id, key),
                )
            return

        await self._send_edit(channel, chat_id, message_id, text)
        self._last_edit[key] = now
        self._pending.pop(key, None)

    async def finalize(
        self,
        channel: str,
        chat_id: str,
        message_id: str,
        text: str,
    ) -> None:
        key = f"{channel}:{chat_id}:{message_id}"

        task = self._flush_tasks.pop(key, None)
        if task and not task.done():
            task.cancel()

        self._pending.pop(key, None)

        event = OutboundEvent.text_reply(
            channel=channel,
            chat_id=chat_id,
            text=text,
        )
        event.edit_message_id = message_id
        event.is_final = True
        event.message_kind = "final"
        await self._bus.publish_outbound(event)

        self._last_edit.pop(key, None)

    async def _delayed_flush(
        self,
        channel: str,
        chat_id: str,
        message_id: str,
        key: str,
    ) -> None:
        await asyncio.sleep(_MIN_EDIT_INTERVAL)
        text = self._pending.pop(key, None)
        if text is not None:
            await self._send_edit(channel, chat_id, message_id, text)
            self._last_edit[key] = time.time()

    async def _send_edit(
        self,
        channel: str,
        chat_id: str,
        message_id: str,
        text: str,
    ) -> None:
        event = OutboundEvent.text_reply(
            channel=channel,
            chat_id=chat_id,
            text=text,
        )
        event.edit_message_id = message_id
        event.is_final = False
        event.message_kind = "streaming"
        await self._bus.publish_outbound(event)
