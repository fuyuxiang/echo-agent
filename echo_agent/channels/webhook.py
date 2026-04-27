"""Webhook channel — HTTP API for external event ingestion."""

from __future__ import annotations

import asyncio
import hashlib
import hmac
import json

from aiohttp import web
from loguru import logger

from echo_agent.bus.events import OutboundEvent
from echo_agent.bus.queue import MessageBus
from echo_agent.channels.base import BaseChannel
from echo_agent.config.schema import WebhookChannelConfig


class WebhookChannel(BaseChannel):
    name = "webhook"

    def __init__(self, config: WebhookChannelConfig, bus: MessageBus):
        super().__init__(config, bus)
        self._app: web.Application | None = None
        self._runner: web.AppRunner | None = None
        self._pending_responses: dict[str, asyncio.Future[str]] = {}

    async def start(self) -> None:
        self._app = web.Application()
        self._app.router.add_post(self.config.path, self._handle_webhook)
        self._app.router.add_get("/health", self._handle_health)
        self._runner = web.AppRunner(self._app)
        await self._runner.setup()
        site = web.TCPSite(self._runner, self.config.host, self.config.port)
        await site.start()
        self._running = True
        self.bus.subscribe_outbound(self.name, self.send)
        logger.info("Webhook channel listening on {}:{}{}", self.config.host, self.config.port, self.config.path)

    async def stop(self) -> None:
        self._running = False
        if self._runner:
            await self._runner.cleanup()

    async def send(self, event: OutboundEvent) -> None:
        future = self._pending_responses.pop(event.reply_to_id or "", None)
        if future and not future.done():
            future.set_result(event.text)

    def _verify_signature(self, body: bytes, signature: str) -> bool:
        if not self.config.secret:
            return True
        expected = hmac.new(
            self.config.secret.encode(), body, hashlib.sha256
        ).hexdigest()
        return hmac.compare_digest(expected, signature)

    async def _handle_webhook(self, request: web.Request) -> web.Response:
        body = await request.read()
        signature = request.headers.get("X-Signature", "")
        if not self._verify_signature(body, signature):
            return web.json_response({"error": "invalid signature"}, status=403)

        try:
            data = json.loads(body)
        except json.JSONDecodeError:
            return web.json_response({"error": "invalid json"}, status=400)

        sender_id = str(data.get("sender_id", "webhook"))
        chat_id = str(data.get("chat_id", "webhook"))
        text = data.get("text", data.get("content", ""))
        if not text:
            return web.json_response({"error": "missing text"}, status=400)

        wait = data.get("wait", False)

        try:
            event = self._build_event(
                sender_id=sender_id,
                chat_id=chat_id,
                text=text,
                metadata=data.get("metadata", {}),
            )
        except PermissionError:
            return web.json_response({"error": "forbidden"}, status=403)

        future: asyncio.Future[str] | None = None
        if wait:
            future = asyncio.get_event_loop().create_future()
            self._pending_responses[event.event_id] = future

        await self.bus.publish_inbound(event)

        if future:
            try:
                result = await asyncio.wait_for(future, timeout=120)
                return web.json_response({"response": result})
            except asyncio.TimeoutError:
                self._pending_responses.pop(event.event_id, None)
                return web.json_response({"error": "timeout"}, status=504)

        return web.json_response({"status": "accepted"})

    async def _handle_health(self, request: web.Request) -> web.Response:
        return web.json_response({"status": "ok", "channel": self.name})
