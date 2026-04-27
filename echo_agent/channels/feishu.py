"""Feishu/Lark channel — Event Subscription webhook + REST API."""

from __future__ import annotations

import hashlib
import json
import time
from base64 import b64decode
from typing import Any

import aiohttp
from aiohttp import web
from loguru import logger

from echo_agent.bus.events import OutboundEvent
from echo_agent.bus.queue import MessageBus
from echo_agent.channels.base import BaseChannel
from echo_agent.config.schema import FeishuChannelConfig

_API_BASE = "https://open.feishu.cn/open-apis"


class FeishuChannel(BaseChannel):
    name = "feishu"

    def __init__(self, config: FeishuChannelConfig, bus: MessageBus):
        super().__init__(config, bus)
        self._app_id = config.app_id
        self._app_secret = config.app_secret
        self._verification_token = config.verification_token
        self._encryption_key = config.encryption_key
        self._session: aiohttp.ClientSession | None = None
        self._runner: web.AppRunner | None = None
        self._tenant_token: str = ""
        self._token_expires: float = 0
        self._seen_ids: set[str] = set()

    async def start(self) -> None:
        self._session = aiohttp.ClientSession()
        await self._refresh_tenant_token()
        app = web.Application()
        app.router.add_post(self.config.webhook_path, self._webhook)
        app.router.add_get("/health", self._health)
        self._runner = web.AppRunner(app)
        await self._runner.setup()
        site = web.TCPSite(self._runner, self.config.host, self.config.port)
        await site.start()
        self._running = True
        self.bus.subscribe_outbound(self.name, self.send)
        logger.info("Feishu channel listening on {}:{}", self.config.host, self.config.port)

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
        await self._ensure_tenant_token()
        chat_id = event.chat_id
        receive_id_type = event.metadata.get("receive_id_type", "chat_id")
        url = f"{_API_BASE}/im/v1/messages?receive_id_type={receive_id_type}"
        payload = {
            "receive_id": chat_id,
            "msg_type": "text",
            "content": json.dumps({"text": text}),
        }
        if event.reply_to_id:
            url = f"{_API_BASE}/im/v1/messages/{event.reply_to_id}/reply"
            payload = {
                "msg_type": "text",
                "content": json.dumps({"text": text}),
            }
        headers = {"Authorization": f"Bearer {self._tenant_token}"}
        try:
            async with self._session.post(url, json=payload, headers=headers) as resp:
                data = await resp.json()
                if data.get("code", 0) != 0:
                    logger.warning("Feishu send error: {} {}", data.get("code"), data.get("msg"))
        except Exception as e:
            logger.error("Feishu send error: {}", e)

    # ── Webhook ──────────────────────────────────────────────────────────────

    async def _webhook(self, request: web.Request) -> web.Response:
        body = await request.read()
        try:
            data = json.loads(body)
        except json.JSONDecodeError:
            return web.json_response({"error": "invalid json"}, status=400)

        if "encrypt" in data and self._encryption_key:
            data = self._decrypt(data["encrypt"])
            if data is None:
                return web.json_response({"error": "decrypt failed"}, status=400)

        if data.get("type") == "url_verification":
            challenge = data.get("challenge", "")
            return web.json_response({"challenge": challenge})

        schema = data.get("schema")
        if schema == "2.0":
            header = data.get("header", {})
            token = header.get("token", "")
            if self._verification_token and token != self._verification_token:
                return web.Response(status=403, text="Forbidden")
            event_id = header.get("event_id", "")
            if event_id in self._seen_ids:
                return web.json_response({"status": "ok"})
            self._seen_ids.add(event_id)
            if len(self._seen_ids) > 10000:
                self._seen_ids = set(list(self._seen_ids)[-5000:])
            event = data.get("event", {})
            event_type = header.get("event_type", "")
            if event_type == "im.message.receive_v1":
                await self._on_message(event)
        else:
            token = data.get("token", "")
            if self._verification_token and token != self._verification_token:
                return web.Response(status=403, text="Forbidden")
            event = data.get("event", {})
            if event.get("type") == "message":
                await self._on_message_v1(event)

        return web.json_response({"status": "ok"})

    async def _on_message(self, event: dict[str, Any]) -> None:
        sender = event.get("sender", {})
        sender_id = sender.get("sender_id", {}).get("open_id", "")
        sender_type = sender.get("sender_type", "")
        if sender_type == "app":
            return

        message = event.get("message", {})
        chat_id = message.get("chat_id", "")
        msg_type = message.get("message_type", "")
        msg_id = message.get("message_id", "")
        chat_type = message.get("chat_type", "")

        text = ""
        media: list[dict[str, str]] = []

        if msg_type == "text":
            try:
                content = json.loads(message.get("content", "{}"))
                text = content.get("text", "")
            except json.JSONDecodeError:
                text = message.get("content", "")
        elif msg_type == "image":
            try:
                content = json.loads(message.get("content", "{}"))
                image_key = content.get("image_key", "")
                if image_key:
                    media.append({"type": "image", "url": image_key})
            except json.JSONDecodeError:
                pass

        if not text and not media:
            return

        receive_id_type = "chat_id" if chat_type in ("group", "p2p") else "open_id"

        await self._handle_message(
            sender_id=sender_id,
            chat_id=chat_id,
            text=text,
            media=media if media else None,
            reply_to_id=msg_id,
            metadata={"chat_type": chat_type, "receive_id_type": receive_id_type},
        )

    async def _on_message_v1(self, event: dict[str, Any]) -> None:
        sender_id = event.get("open_id", "")
        chat_id = event.get("open_chat_id", "")
        msg_type = event.get("msg_type", "")
        text = event.get("text_without_at_bot", "") or event.get("text", "")
        if not text:
            return
        await self._handle_message(
            sender_id=sender_id,
            chat_id=chat_id,
            text=text,
            metadata={"chat_type": event.get("chat_type", ""), "receive_id_type": "chat_id"},
        )

    # ── Encryption ───────────────────────────────────────────────────────────

    def _decrypt(self, encrypted: str) -> dict[str, Any] | None:
        try:
            from hashlib import sha256
            from cryptography.hazmat.primitives.ciphers import Cipher, algorithms, modes
            key = sha256(self._encryption_key.encode()).digest()
            raw = b64decode(encrypted)
            iv = raw[:16]
            cipher = Cipher(algorithms.AES(key), modes.CBC(iv))
            decryptor = cipher.decryptor()
            decrypted = decryptor.update(raw[16:]) + decryptor.finalize()
            pad_len = decrypted[-1]
            decrypted = decrypted[:-pad_len]
            return json.loads(decrypted.decode("utf-8"))
        except ImportError:
            logger.warning("cryptography package not installed, cannot decrypt Feishu events")
            return None
        except Exception as e:
            logger.error("Feishu decrypt failed: {}", e)
            return None

    # ── Tenant token ─────────────────────────────────────────────────────────

    async def _refresh_tenant_token(self) -> None:
        if not self._session:
            return
        url = f"{_API_BASE}/auth/v3/tenant_access_token/internal"
        payload = {"app_id": self._app_id, "app_secret": self._app_secret}
        try:
            async with self._session.post(url, json=payload) as resp:
                data = await resp.json()
                if data.get("code") == 0:
                    self._tenant_token = data.get("tenant_access_token", "")
                    expire = data.get("expire", 7200)
                    self._token_expires = time.time() + expire - 300
                    logger.info("Feishu tenant token refreshed")
                else:
                    logger.error("Feishu token error: {} {}", data.get("code"), data.get("msg"))
        except Exception as e:
            logger.error("Feishu token refresh failed: {}", e)

    async def _ensure_tenant_token(self) -> None:
        if time.time() >= self._token_expires:
            await self._refresh_tenant_token()

    async def _health(self, request: web.Request) -> web.Response:
        return web.json_response({"status": "ok", "channel": self.name})
