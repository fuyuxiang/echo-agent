"""WhatsApp channel — Meta Cloud API webhook + REST.

Features:
- HMAC-SHA256 signature verification on incoming webhooks
- Sender allowlist (empty = accept all)
- Replay protection via message ID dedup
- Group chat support with mention-only policy
- Outbound media (images, documents, audio)
"""

from __future__ import annotations

import hashlib
import hmac
import json
import time
import asyncio
import mimetypes
from pathlib import Path
from typing import Any

from aiohttp import web
import aiohttp
from loguru import logger

from echo_agent.bus.events import OutboundEvent
from echo_agent.bus.queue import MessageBus
from echo_agent.channels.base import BaseChannel, SendResult, DeliveryRejected
from echo_agent.config.schema import WhatsAppChannelConfig
from echo_agent.utils.text import split_message

_GRAPH_API = "https://graph.facebook.com/v21.0"
_REPLAY_WINDOW_SECONDS = 3600
_MAX_SEEN_IDS = 10000


class WhatsAppChannel(BaseChannel):
    name = "whatsapp"
    supports_files = True

    def __init__(self, config: WhatsAppChannelConfig, bus: MessageBus):
        super().__init__(config, bus)
        self._verify_token = config.verify_token
        self._access_token = config.access_token
        self._phone_id = config.phone_number_id
        self._app_secret = config.app_secret
        self._group_policy = getattr(config, "group_policy", "mention")
        self._session: aiohttp.ClientSession | None = None
        self._runner: web.AppRunner | None = None
        self._seen_message_ids: dict[str, float] = {}
        # Bot's own phone number for group mention detection
        self._bot_phone: str = ""

    async def start(self) -> None:
        if not self._app_secret or not self._verify_token:
            raise ValueError("WhatsApp requires app_secret and verify_token for authenticated callbacks")
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
        media_blocks = [
            b for b in event.content
            if b.url and b.type.value != "text"
        ]
        if not text and not media_blocks:
            return SendResult(success=False, error="no content")
        if not self._session:
            return SendResult(success=False, error="no session")

        url = f"{_GRAPH_API}/{self._phone_id}/messages"

        payloads = []
        uploads = {}
        for block in media_blocks:
            media_type = block.type.value if block.type.value in ("image", "audio", "video", "file") else "image"
            wa_type = "document" if media_type == "file" else media_type
            if block.url.startswith(("https://", "http://")):
                payloads.append({"messaging_product": "whatsapp", "to": event.chat_id, "type": wa_type,
                                 wa_type: {"link": block.url}})
            else:
                try:
                    def read_attachment():
                        path = Path(block.url)
                        if not path.is_file():
                            raise ValueError("WhatsApp attachment must be a regular local file")
                        with path.open("rb") as source:
                            data = source.read(self._max_media_download_bytes + 1)
                        if not data or len(data) > self._max_media_download_bytes:
                            raise ValueError("WhatsApp attachment is empty or exceeds the local size limit")
                        return path.name, data
                    name, data = await asyncio.to_thread(read_attachment)
                except (ValueError, OSError) as exc:
                    return SendResult(success=False, error=self.safe_delivery_error(exc))
                index = len(payloads)
                mime = block.mime_type or mimetypes.guess_type(name)[0] or "application/octet-stream"
                uploads[index] = (name, mime, data)
                # Stable local content identity precedes the remote upload ID.
                payloads.append({"_upload": True, "name": name, "mime": mime, "source": block.url,
                                 "digest": hashlib.sha256(data).hexdigest(), "type": wa_type})

        if text:
            payloads.extend({"messaging_product": "whatsapp", "to": event.chat_id, "type": "text",
                             "text": {"body": chunk}}
                            for chunk in split_message(text, 4000, preserve_whitespace=True))

        async def send_part(index, part):
            payload = json.loads(part)
            if payload.get("_upload"):
                name, mime, data = uploads[index]
                form = aiohttp.FormData()
                form.add_field("messaging_product", "whatsapp")
                form.add_field("type", mime)
                form.add_field("file", data, filename=name, content_type=mime)
                try:
                    async with self._session.post(f"{_GRAPH_API}/{self._phone_id}/media", data=form) as response:
                        uploaded = await response.json()
                        media_id = uploaded.get("id") if isinstance(uploaded, dict) else None
                        if response.status >= 400 or not isinstance(media_id, str) or not media_id:
                            raise ValueError(f"WhatsApp upload failed (HTTP {response.status})")
                except Exception as exc:
                    # Uploading a blob alone cannot deliver a message. A failed
                    # upload can safely release the pending message checkpoint.
                    raise DeliveryRejected(self.safe_delivery_error(exc)) from exc
                wa_type = payload["type"]
                payload = {"messaging_product": "whatsapp", "to": event.chat_id, "type": wa_type,
                           wa_type: {"id": media_id}}
            return await self._post_payload(url, payload)
        return await self.send_parts(event, [json.dumps(payload, sort_keys=True) for payload in payloads], send_part)

    async def _post_payload(self, url: str, payload: dict[str, Any]) -> SendResult:
        try:
            async with self._session.post(url, json=payload) as resp:
                if resp.status >= 400:
                    detail = self.safe_delivery_error(RuntimeError((await resp.text())[:200]))
                    return SendResult(success=False, error=f"WhatsApp HTTP {resp.status}: {detail}",
                                      delivery_unknown=resp.status >= 500)
                data = await resp.json()
                messages = data.get("messages") if isinstance(data, dict) else None
                receipt = messages[0].get("id") if isinstance(messages, list) and messages and isinstance(messages[0], dict) else None
                if not isinstance(receipt, str) or not receipt:
                    return SendResult(success=False, error="WhatsApp send receipt missing", delivery_unknown=True)
                return SendResult(success=True, message_id=receipt)
        except Exception as exc:
            return SendResult(success=False, error=self.safe_delivery_error(exc), delivery_unknown=True)

    async def _send_media(
        self, url: str, to: str, media_type: str, media_url: str, *, caption: str | None = None,
    ) -> SendResult:
        """Send a media message (image, document, audio)."""
        if not self._session:
            return SendResult(success=False, error="no session")

        wa_type_map = {"image": "image", "file": "document", "audio": "audio", "video": "video"}
        wa_type = wa_type_map.get(media_type, "image")

        payload: dict[str, Any] = {
            "messaging_product": "whatsapp",
            "to": to,
            "type": wa_type,
            wa_type: {"link": media_url},
        }
        if caption and wa_type in ("image", "document"):
            payload[wa_type]["caption"] = caption

        return await self._post_payload(url, payload)

    async def _verify(self, request: web.Request) -> web.Response:
        mode = request.query.get("hub.mode")
        token = request.query.get("hub.verify_token")
        challenge = request.query.get("hub.challenge", "")
        if self._verify_token and mode == "subscribe" and token == self._verify_token:
            return web.Response(text=challenge)
        return web.Response(status=403, text="Forbidden")

    async def _webhook(self, request: web.Request) -> web.Response:
        if not self._app_secret:
            return web.json_response({"error": "webhook authentication is not configured"}, status=403)
        # Read raw body first — needed for HMAC verification before JSON parsing
        raw_body = await request.read()

        # HMAC signature verification
        if self._app_secret:
            signature = request.headers.get("X-Hub-Signature-256", "")
            if not self._verify_signature(raw_body, signature):
                logger.warning("WhatsApp webhook signature verification failed")
                return web.json_response({"error": "invalid signature"}, status=403)

        try:
            import json as _json
            data = _json.loads(raw_body)
        except Exception as e:
            logger.debug("Invalid JSON in WhatsApp webhook: {}", e)
            return web.json_response({"error": "invalid json"}, status=400)
        if not isinstance(data, dict):
            return web.json_response({"error": "json body must be an object"}, status=400)

        for entry in data.get("entry", []):
            for change in entry.get("changes", []):
                value = change.get("value", {})
                # Extract metadata for group detection
                metadata = value.get("metadata", {})
                self._bot_phone = metadata.get("phone_number", self._bot_phone)

                contacts = value.get("contacts", [])
                for msg in value.get("messages", []):
                    async with self.bus.delivery_lock(f"ingress:whatsapp:{msg.get('id', '')}"):
                        if await self._process_message(msg, value, contacts) is False:
                            return web.json_response({"error": "runtime busy; retry later"}, status=503)

        return web.json_response({"status": "ok"})

    def _verify_signature(self, raw_body: bytes, signature: str) -> bool:
        """Verify HMAC-SHA256 signature from Meta.

        *raw_body* is the unmodified request payload; *signature* is the
        ``X-Hub-Signature-256`` header value (``sha256=<hex>``).
        """
        if not signature.startswith("sha256="):
            return False
        expected_hex = signature[7:]  # Remove "sha256=" prefix
        computed = hmac.new(
            self._app_secret.encode("utf-8"),
            raw_body,
            hashlib.sha256,
        ).hexdigest()
        return hmac.compare_digest(computed, expected_hex)

    async def _process_message(self, msg: dict[str, Any], value: dict[str, Any], contacts: list[dict]) -> bool:
        msg_id = msg.get("id", "")

        # Replay protection: skip already-processed messages
        if msg_id and self._is_replay(msg_id):
            logger.debug("WhatsApp replay detected: {}", msg_id)
            return True

        sender = msg.get("from", "")
        msg_type = msg.get("type", "")

        # Sender allowlist check
        if not self.is_allowed(sender):
            logger.debug("WhatsApp message from non-allowed sender: {}", sender)
            return True

        # Group detection
        is_group = msg.get("from", "").startswith("group:")
        chat_id = sender
        if is_group:
            # For group messages, chat_id should be the group ID
            chat_id = msg.get("chat_id", sender)

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
        elif msg_type == "video":
            video = msg.get("video", {})
            text = video.get("caption", "")
            media_id = video.get("id", "")
            if media_id:
                local_path = await self._download_whatsapp_media(media_id)
                if local_path:
                    media.append({"type": "video", "url": local_path})

        if not text and not media:
            return True

        # Group mention policy
        if is_group and self._group_policy == "mention":
            # Check if bot is mentioned (WhatsApp uses @phone_number format)
            if f"@{self._bot_phone}" not in text:
                return True

        inbound = await self._handle_message(
            sender_id=sender,
            chat_id=chat_id,
            text=text,
            media=media if media else None,
            metadata={"message_type": msg_type, "message_id": msg_id},
            is_group=is_group,
            event_id=self._native_event_id(chat_id, msg_id),
        )
        if inbound is None and msg_id:
            self._seen_message_ids.pop(msg_id, None)
        return inbound is not None

    def _is_replay(self, message_id: str) -> bool:
        """Check if message ID was already processed (replay protection)."""
        now = time.time()
        if len(self._seen_message_ids) > _MAX_SEEN_IDS:
            cutoff = now - _REPLAY_WINDOW_SECONDS
            self._seen_message_ids = {
                mid: ts for mid, ts in self._seen_message_ids.items()
                if ts > cutoff
            }

        if message_id in self._seen_message_ids:
            return True

        self._seen_message_ids[message_id] = now
        return False

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
            data = await self._fetch_with_limit(
                self._session, download_url, max_bytes=self._max_media_download_bytes,
            )
            if data is None:
                return b""
            return data

        return await self._resolve_media_to_cache(media_id, "whatsapp", fetch)

    async def _health(self, request: web.Request) -> web.Response:
        return web.json_response({"status": "ok", "channel": self.name})
