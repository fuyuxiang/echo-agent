"""WhatsApp channel — Meta Cloud API webhook + REST."""

from __future__ import annotations

import json
from typing import Any

from aiohttp import web
import aiohttp
from loguru import logger

from echo_agent.bus.events import OutboundEvent
from echo_agent.bus.queue import MessageBus
from echo_agent.channels.base import BaseChannel
from echo_agent.config.schema import WhatsAppChannelConfig

_GRAPH_API = "https://graph.facebook.com/v21.0"


class WhatsAppChannel(BaseChannel):
    name = "whatsapp"

    def __init__(self, config: WhatsAppChannelConfig, bus: MessageBus):
        super().__init__(config, bus)
        self._verify_token = config.verify_token
        self._access_token = config.access_token
        self._phone_id = config.phone_number_id
        self._session: aiohttp.ClientSession | None = None
        self._runner: web.AppRunner | None = None

    async def start(self) -> None:
        self._session = aiohttp.ClientSession(headers={
            "Authorization": f"Bearer {self._access_token}",
        })
        app = web.Application()
        app.router.add_get(self.config.webhook_path, self._verify)
        app.router.add_post(self.config.webhook_path, self._webhook)
        app.router.add_get("/health", self._health)
        self._runner = web.AppRunner(app)
        await self._runner.setup()
        site = web.TCPSite(self._runner, self.config.host, self.config.port)
        await site.start()
        self._running = True
        self.bus.subscribe_outbound(self.name, self.send)
        logger.info("WhatsApp channel listening on {}:{}", self.config.host, self.config.port)

    async def stop(self) -> None:
        self._running = False
        if self._runner:
            await self._runner.cleanup()
        if self._session:
            await self._session.close()

    async def send(self, event: OutboundEvent) -> None:
        text = event.text or ""
        if not text or not self._session:
            return
        url = f"{_GRAPH_API}/{self._phone_id}/messages"
        payload = {
            "messaging_product": "whatsapp",
            "to": event.chat_id,
            "type": "text",
            "text": {"body": text},
        }
        try:
            async with self._session.post(url, json=payload) as resp:
                if resp.status >= 400:
                    body = await resp.text()
                    logger.warning("WhatsApp send failed ({}): {}", resp.status, body[:200])
        except Exception as e:
            logger.error("WhatsApp send error: {}", e)

    async def _verify(self, request: web.Request) -> web.Response:
        mode = request.query.get("hub.mode")
        token = request.query.get("hub.verify_token")
        challenge = request.query.get("hub.challenge", "")
        if mode == "subscribe" and token == self._verify_token:
            return web.Response(text=challenge)
        return web.Response(status=403, text="Forbidden")

    async def _webhook(self, request: web.Request) -> web.Response:
        try:
            data = await request.json()
        except Exception as e:
            logger.debug("Invalid JSON in WhatsApp webhook: {}", e)
            return web.json_response({"error": "invalid json"}, status=400)

        for entry in data.get("entry", []):
            for change in entry.get("changes", []):
                value = change.get("value", {})
                for msg in value.get("messages", []):
                    await self._process_message(msg, value)

        return web.json_response({"status": "ok"})

    async def _process_message(self, msg: dict[str, Any], value: dict[str, Any]) -> None:
        sender = msg.get("from", "")
        msg_type = msg.get("type", "")

        text = ""
        media: list[dict[str, str]] = []

        if msg_type == "text":
            text = msg.get("text", {}).get("body", "")
        elif msg_type == "image":
            img = msg.get("image", {})
            text = img.get("caption", "")
            media_id = img.get("id", "")
            if media_id:
                media.append({"type": "image", "url": media_id})
        elif msg_type == "document":
            doc = msg.get("document", {})
            text = doc.get("caption", "")
            media_id = doc.get("id", "")
            if media_id:
                media.append({"type": "file", "url": media_id})
        elif msg_type == "audio":
            audio = msg.get("audio", {})
            media_id = audio.get("id", "")
            if media_id:
                media.append({"type": "audio", "url": media_id})

        if not text and not media:
            return

        await self._handle_message(
            sender_id=sender,
            chat_id=sender,
            text=text,
            media=media if media else None,
            metadata={"message_type": msg_type},
        )

    async def _health(self, request: web.Request) -> web.Response:
        return web.json_response({"status": "ok", "channel": self.name})
