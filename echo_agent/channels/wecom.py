"""WeCom (Enterprise WeChat) channel — webhook callback + REST API."""

from __future__ import annotations

import hashlib
import time
import xml.etree.ElementTree as ET

import aiohttp
from aiohttp import web
from loguru import logger

from echo_agent.bus.events import OutboundEvent
from echo_agent.bus.queue import MessageBus
from echo_agent.channels.base import BaseChannel
from echo_agent.config.schema import WeComChannelConfig

_API_BASE = "https://qyapi.weixin.qq.com/cgi-bin"


class WeComChannel(BaseChannel):
    name = "wecom"

    def __init__(self, config: WeComChannelConfig, bus: MessageBus):
        super().__init__(config, bus)
        self._corp_id = config.corp_id
        self._agent_id = config.agent_id
        self._secret = config.secret
        self._token = config.token
        self._session: aiohttp.ClientSession | None = None
        self._runner: web.AppRunner | None = None
        self._access_token: str = ""
        self._token_expires: float = 0

    async def start(self) -> None:
        self._session = aiohttp.ClientSession()
        await self._refresh_token()
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
        logger.info("WeCom channel listening on {}:{}", self.config.host, self.config.port)

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
        await self._ensure_token()
        url = f"{_API_BASE}/message/send?access_token={self._access_token}"
        payload = {
            "touser": event.chat_id,
            "msgtype": "text",
            "agentid": int(self._agent_id) if self._agent_id.isdigit() else 0,
            "text": {"content": text},
        }
        try:
            async with self._session.post(url, json=payload) as resp:
                data = await resp.json()
                if data.get("errcode"):
                    logger.warning("WeCom send error: {} {}", data.get("errcode"), data.get("errmsg"))
        except Exception as e:
            logger.error("WeCom send error: {}", e)

    def _check_signature(self, signature: str, timestamp: str, nonce: str) -> bool:
        items = sorted([self._token, timestamp, nonce])
        digest = hashlib.sha1("".join(items).encode()).hexdigest()
        return digest == signature

    async def _verify(self, request: web.Request) -> web.Response:
        signature = request.query.get("msg_signature", "")
        timestamp = request.query.get("timestamp", "")
        nonce = request.query.get("nonce", "")
        echostr = request.query.get("echostr", "")
        if self._check_signature(signature, timestamp, nonce):
            return web.Response(text=echostr)
        return web.Response(status=403, text="Forbidden")

    async def _webhook(self, request: web.Request) -> web.Response:
        body = await request.text()
        try:
            root = ET.fromstring(body)
        except ET.ParseError:
            return web.Response(text="success")

        msg_type = root.findtext("MsgType", "")
        from_user = root.findtext("FromUserName", "")
        text = ""

        if msg_type == "text":
            text = root.findtext("Content", "")
        elif msg_type == "event":
            event_type = root.findtext("Event", "")
            if event_type == "subscribe":
                text = "/start"
            else:
                return web.Response(text="success")
        else:
            return web.Response(text="success")

        if not text:
            return web.Response(text="success")

        await self._handle_message(
            sender_id=from_user, chat_id=from_user, text=text,
            metadata={"msg_type": msg_type},
        )
        return web.Response(text="success")

    async def _refresh_token(self) -> None:
        if not self._session:
            return
        url = f"{_API_BASE}/gettoken?corpid={self._corp_id}&corpsecret={self._secret}"
        try:
            async with self._session.get(url) as resp:
                data = await resp.json()
                if data.get("errcode") == 0:
                    self._access_token = data.get("access_token", "")
                    self._token_expires = time.time() + data.get("expires_in", 7200) - 300
                    logger.info("WeCom access token refreshed")
                else:
                    logger.error("WeCom token error: {} {}", data.get("errcode"), data.get("errmsg"))
        except Exception as e:
            logger.error("WeCom token refresh failed: {}", e)

    async def _ensure_token(self) -> None:
        if time.time() >= self._token_expires:
            await self._refresh_token()

    async def _health(self, request: web.Request) -> web.Response:
        return web.json_response({"status": "ok", "channel": self.name})
