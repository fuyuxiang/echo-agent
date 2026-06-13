"""WhatsApp channel — Meta Cloud API webhook + REST."""

from __future__ import annotations

from typing import Any

from aiohttp import web
import aiohttp
from loguru import logger

from echo_agent.bus.events import OutboundEvent
from echo_agent.bus.queue import MessageBus
from echo_agent.channels.base import BaseChannel, SendResult
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

    async def send(self, event: OutboundEvent) -> SendResult | None:
        if not self.should_deliver(event):
            return SendResult(success=True, skipped=True)
        text = event.text or ""
        if not text or not self._session:
            return SendResult(success=False, error="no text or no session")
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
                    return SendResult(success=False, error=body[:200])
        except Exception as e:
            logger.error("WhatsApp send error: {}", e)
            return SendResult(success=False, error=str(e))
        return SendResult(success=True)

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
                local_path = await self._download_whatsapp_media(media_id)
                if local_path:
                    media.append({"type": "image", "url": local_path})
        elif msg_type == "document":
            doc = msg.get("document", {})
            text = doc.get("caption", "")
            media_id = doc.get("id", "")
            if media_id:
                local_path = await self._download_whatsapp_media(media_id)
                if local_path:
                    media.append({"type": "file", "url": local_path})
        elif msg_type == "audio":
            audio = msg.get("audio", {})
            media_id = audio.get("id", "")
            if media_id:
                local_path = await self._download_whatsapp_media(media_id)
                if local_path:
                    media.append({"type": "audio", "url": local_path})

        if not text and not media:
            return

        await self._handle_message(
            sender_id=sender,
            chat_id=sender,
            text=text,
            media=media if media else None,
            metadata={"message_type": msg_type},
        )

    async def _download_whatsapp_media(self, media_id: str) -> str | None:
        """Download a WhatsApp media file via the two-step Graph API flow."""
        async def fetch() -> bytes:
            if not self._session:
                raise RuntimeError("no session")
            meta_url = f"{_GRAPH_API}/{media_id}"
            async with self._session.get(meta_url) as resp:
                if resp.status != 200:
                    raise RuntimeError(f"Graph API media info failed ({resp.status})")
                info = await resp.json()
            download_url = info.get("url", "")
            if not download_url:
                raise RuntimeError("Graph API returned no download URL")
            async with self._session.get(download_url) as resp:
                if resp.status != 200:
                    raise RuntimeError(f"media download failed ({resp.status})")
                return await resp.read()

        return await self._resolve_media_to_cache(media_id, "whatsapp", fetch)

    async def _health(self, request: web.Request) -> web.Response:
        return web.json_response({"status": "ok", "channel": self.name})
