"""Feishu/Lark channel — Event Subscription webhook + REST API."""

from __future__ import annotations

import json
import time
import hashlib
import hmac
from base64 import b64decode
from typing import Any

import aiohttp
from aiohttp import web
from loguru import logger

from echo_agent.bus.events import OutboundEvent
from echo_agent.bus.queue import MessageBus
from echo_agent.channels.base import BaseChannel, SendResult
from echo_agent.utils.text import split_encoded_message
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

        group_policy = getattr(config, "group_policy", "mention")
        if not isinstance(group_policy, str) or group_policy not in ("mention", "all"):
            group_policy = "mention"
            if isinstance(getattr(config, "group_policy", None), str):
                raise ValueError(
                    f"Invalid feishu group_policy '{config.group_policy}': "
                    f"must be 'mention' or 'all'"
                )
        self._group_policy = group_policy

        bot_open_id = getattr(config, "bot_open_id", None)
        bot_open_id = bot_open_id if isinstance(bot_open_id, str) else ""
        if not bot_open_id:
            logger.warning(
                "Feishu channel: 'bot_open_id' not configured; "
                "group mention filtering will fall back to app_id, "
                "which may not match correctly"
            )
            bot_open_id = self._app_id
        self._bot_open_id = bot_open_id

        self._session: aiohttp.ClientSession | None = None
        self._runner: web.AppRunner | None = None
        self._tenant_token: str = ""
        self._token_expires: float = 0
        self._seen_ids: set[str] = set()

    async def start(self) -> None:
        if not self._verification_token and not self._encryption_key:
            raise ValueError("Feishu requires verification_token or encryption_key for authenticated callbacks")
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

    async def send(self, event: OutboundEvent) -> SendResult | None:
        if not self.should_deliver(event):
            return SendResult(success=True, skipped=True)
        text = event.text or ""
        if not text or not self._session:
            return SendResult(success=False, error="no text or no session")
        await self._ensure_tenant_token()
        chat_id = event.chat_id
        receive_id_type = event.metadata.get("receive_id_type", "chat_id")
        url = f"{_API_BASE}/im/v1/messages?receive_id_type={receive_id_type}"
        def encoded_size(part):
            return len(json.dumps(json.dumps({"text": part}, ensure_ascii=False)).encode())

        async def send_part(index, part):
            part_url = url
            payload = {"receive_id": chat_id, "msg_type": "text",
                       "content": json.dumps({"text": part}, ensure_ascii=False)}
            if event.reply_to_id:
                part_url = f"{_API_BASE}/im/v1/messages/{event.reply_to_id}/reply"
                payload.pop("receive_id")
            headers = {"Authorization": f"Bearer {self._tenant_token}"}
            try:
                async with self._session.post(part_url, json=payload, headers=headers) as resp:
                    data = await resp.json()
                    if resp.status >= 400 or data.get("code", 0) != 0:
                        return SendResult(success=False, error=f"{data.get('code')}: {data.get('msg')}",
                                          delivery_unknown=resp.status >= 500)
                    message_id = str((data.get("data") or {}).get("message_id") or "")
                    if not message_id:
                        return SendResult(success=False, error="Feishu send receipt missing", delivery_unknown=True)
                    return SendResult(success=True, message_id=message_id)
            except Exception as exc:
                return SendResult(success=False, error=type(exc).__name__, delivery_unknown=True)
        # A conservative budget includes both JSON layers, including Unicode escaping.
        return await self.send_parts(event, split_encoded_message(text, 8000, encoded_size), send_part)

    # ── Webhook ──────────────────────────────────────────────────────────────

    async def _webhook(self, request: web.Request) -> web.Response:
        if not self._verification_token and not self._encryption_key:
            return web.Response(status=403, text="Webhook authentication is not configured")
        body = await request.read()
        if self._encryption_key:
            timestamp = str(request.headers.get("X-Lark-Request-Timestamp", ""))
            nonce = str(request.headers.get("X-Lark-Request-Nonce", ""))
            signature = str(request.headers.get("X-Lark-Signature", ""))
            expected = hashlib.sha256((timestamp + nonce + self._encryption_key).encode() + body).hexdigest()
            if not timestamp or not nonce or not hmac.compare_digest(expected, signature):
                return web.Response(status=403, text="Invalid callback signature")
        try:
            data = json.loads(body)
        except json.JSONDecodeError:
            return web.json_response({"error": "invalid json"}, status=400)
        if not isinstance(data, dict):
            return web.json_response({"error": "json body must be an object"}, status=400)

        if "encrypt" in data and self._encryption_key:
            data = self._decrypt(data["encrypt"])
            if data is None:
                return web.json_response({"error": "decrypt failed"}, status=400)
        if not isinstance(data, dict) or not isinstance(data.get("header", {}), dict):
            return web.json_response({"error": "invalid callback object"}, status=400)

        token = (data.get("header") or {}).get("token", "") if data.get("schema") == "2.0" else data.get("token", "")
        if self._verification_token and not hmac.compare_digest(str(token), self._verification_token):
            return web.Response(status=403, text="Forbidden")

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
            if event_id and event_id in self._seen_ids:
                return web.json_response({"status": "ok"})
            event = data.get("event", {})
            event_type = header.get("event_type", "")
            if event_type == "im.message.receive_v1":
                if await self._on_message(event) is False:
                    return web.json_response({"error": "runtime busy; retry later"}, status=503)
            if event_id:
                self._seen_ids.add(event_id)
                if len(self._seen_ids) > 10000:
                    self._seen_ids = set(list(self._seen_ids)[-5000:])
        else:
            token = data.get("token", "")
            if self._verification_token and token != self._verification_token:
                return web.Response(status=403, text="Forbidden")
            event = data.get("event", {})
            if event.get("type") == "message":
                if await self._on_message_v1(event) is False:
                    return web.json_response({"error": "runtime busy; retry later"}, status=503)

        return web.json_response({"status": "ok"})

    async def _on_message(self, event: dict[str, Any]) -> bool:
        sender = event.get("sender", {})
        sender_id = sender.get("sender_id", {}).get("open_id", "")
        sender_type = sender.get("sender_type", "")
        if sender_type == "app":
            return True
        if not self.is_allowed(sender_id):
            return True

        message = event.get("message", {})
        chat_id = message.get("chat_id", "")
        msg_type = message.get("message_type", "")
        msg_id = message.get("message_id", "")
        chat_type = message.get("chat_type", "")

        if chat_type == "group" and self._group_policy == "mention":
            mentions = message.get("mentions") or []
            bot_mentioned = any(
                m.get("id", {}).get("open_id") == self._bot_open_id
                for m in mentions
            )
            if not bot_mentioned and not await self.is_clarification_reply(sender_id, chat_id):
                return True

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
                    local_path = await self._download_feishu_image(image_key, msg_id)
                    if local_path:
                        media.append({"type": "image", "url": local_path})
                    else:
                        logger.warning("Feishu image download failed, skipping: {}", image_key[:30])
            except json.JSONDecodeError as e:
                # Do not log the malformed message body: it may contain private
                # content. The message id and parser reason are sufficient.
                logger.debug("Feishu image metadata is invalid for message {}: {}", msg_id, e)

        if not text and not media:
            return True

        receive_id_type = "chat_id" if chat_type in ("group", "p2p") else "open_id"

        inbound = await self._handle_message(
            sender_id=sender_id,
            chat_id=chat_id,
            text=text,
            media=media if media else None,
            reply_to_id=msg_id,
            metadata={"chat_type": chat_type, "receive_id_type": receive_id_type},
            is_group=(chat_type == "group"),
            event_id=self._native_event_id(chat_id, msg_id),
        )
        return inbound is not None

    async def _on_message_v1(self, event: dict[str, Any]) -> bool:
        sender_id = event.get("open_id", "")
        chat_id = event.get("open_chat_id", "")
        text = event.get("text_without_at_bot", "") or event.get("text", "")
        if not text:
            return True
        if not self.is_allowed(sender_id):
            return True
        inbound = await self._handle_message(
            sender_id=sender_id,
            chat_id=chat_id,
            text=text,
            metadata={"chat_type": event.get("chat_type", ""), "receive_id_type": "chat_id"},
            is_group=(event.get("chat_type", "") == "group"),
            event_id=self._native_event_id(chat_id, event.get("msg_id") or event.get("message_id")),
        )
        return inbound is not None

    async def _download_feishu_image(self, image_key: str, msg_id: str) -> str | None:
        """Download a Feishu image by image_key via the message resource API."""
        async def fetch() -> bytes:
            await self._ensure_tenant_token()
            url = (
                f"{_API_BASE}/im/v1/messages/{msg_id}"
                f"/resources/{image_key}?type=image"
            )
            headers = {"Authorization": f"Bearer {self._tenant_token}"}
            if not self._session:
                raise RuntimeError("no session")
            data = await self._fetch_with_limit(
                self._session, url, max_bytes=self._max_media_download_bytes, headers=headers,
            )
            if data is None:
                return b""
            return data

        return await self._resolve_media_to_cache(image_key, "feishu", fetch, suffix=".jpg")

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
