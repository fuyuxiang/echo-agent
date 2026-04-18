"""WeChat channel — Official Account API with webhook + REST."""

from __future__ import annotations

import hashlib
import time
import xml.etree.ElementTree as ET
from typing import Any

import aiohttp
from aiohttp import web
from loguru import logger

from echo_agent.bus.events import OutboundEvent
from echo_agent.bus.queue import MessageBus
from echo_agent.channels.base import BaseChannel
from echo_agent.config.schema import WeChatChannelConfig

_API_BASE = "https://api.weixin.qq.com/cgi-bin"


class WeChatChannel(BaseChannel):
    name = "wechat"

    def __init__(self, config: WeChatChannelConfig, bus: MessageBus):
        super().__init__(config, bus)
        self._app_id = config.app_id
        self._app_secret = config.app_secret
        self._token = config.token
        self._session: aiohttp.ClientSession | None = None
        self._runner: web.AppRunner | None = None
        self._access_token: str = ""
        self._token_expires: float = 0

    async def start(self) -> None:
        self._session = aiohttp.ClientSession()
        await self._refresh_access_token()
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
        logger.info("WeChat channel listening on {}:{}", self.config.host, self.config.port)

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
        await self._ensure_access_token()
        url = f"{_API_BASE}/message/custom/send?access_token={self._access_token}"
        payload = {
            "touser": event.chat_id,
            "msgtype": "text",
            "text": {"content": text},
        }
        try:
            async with self._session.post(url, json=payload) as resp:
                data = await resp.json()
                if data.get("errcode"):
                    logger.warning("WeChat send error: {} {}", data.get("errcode"), data.get("errmsg"))
        except Exception as e:
            logger.error("WeChat send error: {}", e)

    # ── Webhook ──────────────────────────────────────────────────────────────

    def _check_signature(self, signature: str, timestamp: str, nonce: str) -> bool:
        items = sorted([self._token, timestamp, nonce])
        digest = hashlib.sha1("".join(items).encode()).hexdigest()
        return digest == signature

    async def _verify(self, request: web.Request) -> web.Response:
        signature = request.query.get("signature", "")
        timestamp = request.query.get("timestamp", "")
        nonce = request.query.get("nonce", "")
        echostr = request.query.get("echostr", "")
        if self._check_signature(signature, timestamp, nonce):
            return web.Response(text=echostr)
        return web.Response(status=403, text="Forbidden")

    async def _webhook(self, request: web.Request) -> web.Response:
        signature = request.query.get("signature", "")
        timestamp = request.query.get("timestamp", "")
        nonce = request.query.get("nonce", "")
        if not self._check_signature(signature, timestamp, nonce):
            return web.Response(status=403, text="Forbidden")

        body = await request.text()
        try:
            root = ET.fromstring(body)
        except ET.ParseError:
            return web.Response(text="success")

        msg_type = root.findtext("MsgType", "")
        from_user = root.findtext("FromUserName", "")
        text = ""
        media: list[dict[str, str]] = []

        if msg_type == "text":
            text = root.findtext("Content", "")
        elif msg_type == "image":
            pic_url = root.findtext("PicUrl", "")
            if pic_url:
                media.append({"type": "image", "url": pic_url})
        elif msg_type == "voice":
            recognition = root.findtext("Recognition", "")
            if recognition:
                text = recognition
            media_id = root.findtext("MediaId", "")
            if media_id:
                media.append({"type": "audio", "url": media_id})
        elif msg_type == "event":
            event_type = root.findtext("Event", "")
            if event_type == "subscribe":
                text = "/start"
            else:
                return web.Response(text="success")
        else:
            return web.Response(text="success")

        if not text and not media:
            return web.Response(text="success")

        await self._handle_message(
            sender_id=from_user,
            chat_id=from_user,
            text=text,
            media=media if media else None,
            metadata={"msg_type": msg_type},
        )
        return web.Response(text="success")

    # ── Access token ─────────────────────────────────────────────────────────

    async def _refresh_access_token(self) -> None:
        if not self._session:
            return
        url = f"{_API_BASE}/token?grant_type=client_credential&appid={self._app_id}&secret={self._app_secret}"
        try:
            async with self._session.get(url) as resp:
                data = await resp.json()
                if "access_token" in data:
                    self._access_token = data["access_token"]
                    self._token_expires = time.time() + data.get("expires_in", 7200) - 300
                    logger.info("WeChat access token refreshed")
                else:
                    logger.error("WeChat token error: {}", data)
        except Exception as e:
            logger.error("WeChat token refresh failed: {}", e)

    async def _ensure_access_token(self) -> None:
        if time.time() >= self._token_expires:
            await self._refresh_access_token()

    async def _health(self, request: web.Request) -> web.Response:
        return web.json_response({"status": "ok", "channel": self.name})
