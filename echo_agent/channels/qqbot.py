"""QQ Bot channel — Official API v2 WebSocket gateway."""

from __future__ import annotations

import asyncio
import json
import time
from typing import Any

import aiohttp
from loguru import logger

from echo_agent.bus.events import OutboundEvent
from echo_agent.bus.queue import MessageBus
from echo_agent.channels.base import BaseChannel
from echo_agent.config.schema import QQBotChannelConfig

_API_BASE = "https://api.sgroup.qq.com"
_SANDBOX_API = "https://sandbox.api.sgroup.qq.com"


class QQBotChannel(BaseChannel):
    name = "qqbot"

    def __init__(self, config: QQBotChannelConfig, bus: MessageBus):
        super().__init__(config, bus)
        self._app_id = config.app_id
        self._app_secret = config.app_secret
        self._sandbox = config.sandbox
        self._session: aiohttp.ClientSession | None = None
        self._ws: aiohttp.ClientWebSocketResponse | None = None
        self._ws_task: asyncio.Task | None = None
        self._heartbeat_task: asyncio.Task | None = None
        self._access_token: str = ""
        self._token_expires: float = 0
        self._seq: int | None = None
        self._session_id: str = ""
        self._heartbeat_interval: float = 41.25

    @property
    def _api_base(self) -> str:
        return _SANDBOX_API if self._sandbox else _API_BASE

    async def start(self) -> None:
        self._session = aiohttp.ClientSession()
        await self._refresh_token()
        self._running = True
        self.bus.subscribe_outbound(self.name, self.send)
        self._ws_task = asyncio.create_task(self._ws_loop())
        logger.info("QQBot channel started")

    async def stop(self) -> None:
        self._running = False
        if self._heartbeat_task:
            self._heartbeat_task.cancel()
        if self._ws_task:
            self._ws_task.cancel()
            try:
                await self._ws_task
            except asyncio.CancelledError:
                pass
        if self._ws and not self._ws.closed:
            await self._ws.close()
        if self._session:
            await self._session.close()

    async def send(self, event: OutboundEvent) -> None:
        text = event.text or ""
        if not text or not self._session:
            return
        await self._ensure_token()
        msg_type = event.metadata.get("msg_type", "group")
        msg_id = event.reply_to_id or ""

        if msg_type == "channel":
            url = f"{self._api_base}/channels/{event.chat_id}/messages"
            payload: dict[str, Any] = {"content": text}
            if msg_id:
                payload["msg_id"] = msg_id
        elif msg_type == "c2c":
            url = f"{self._api_base}/v2/users/{event.chat_id}/messages"
            payload = {"content": text, "msg_type": 0}
            if msg_id:
                payload["msg_id"] = msg_id
        else:
            url = f"{self._api_base}/v2/groups/{event.chat_id}/messages"
            payload = {"content": text, "msg_type": 0}
            if msg_id:
                payload["msg_id"] = msg_id

        headers = self._auth_headers()
        try:
            async with self._session.post(url, json=payload, headers=headers) as resp:
                if resp.status >= 400:
                    body = await resp.text()
                    logger.warning("QQBot send failed ({}): {}", resp.status, body[:200])
        except Exception as e:
            logger.error("QQBot send error: {}", e)

    # ── WebSocket ────────────────────────────────────────────────────────────

    async def _ws_loop(self) -> None:
        while self._running:
            try:
                await self._connect_and_listen()
            except asyncio.CancelledError:
                break
            except Exception as e:
                logger.error("QQBot WS error: {}", e)
            if self._running:
                await asyncio.sleep(5)

    async def _connect_and_listen(self) -> None:
        if not self._session:
            return
        await self._ensure_token()
        gw_url = await self._get_gateway()
        if not gw_url:
            logger.error("Failed to get QQBot gateway URL")
            await asyncio.sleep(10)
            return

        self._ws = await self._session.ws_connect(gw_url)

        async for msg in self._ws:
            if msg.type == aiohttp.WSMsgType.TEXT:
                await self._handle_ws_message(json.loads(msg.data))
            elif msg.type in (aiohttp.WSMsgType.CLOSED, aiohttp.WSMsgType.ERROR):
                break

    async def _handle_ws_message(self, data: dict[str, Any]) -> None:
        op = data.get("op")
        s = data.get("s")
        if s is not None:
            self._seq = s
        t = data.get("t")
        d = data.get("d", {})

        if op == 10:  # HELLO
            self._heartbeat_interval = d.get("heartbeat_interval", 41250) / 1000
            if self._heartbeat_task:
                self._heartbeat_task.cancel()
            self._heartbeat_task = asyncio.create_task(self._heartbeat_loop())
            if self._session_id:
                await self._send_ws({"op": 6, "d": {"session_id": self._session_id, "seq": self._seq}})
            else:
                await self._send_ws({"op": 2, "d": {
                    "token": f"QQBot {self._access_token}",
                    "intents": (1 << 30) | (1 << 25) | (1 << 0),
                    "shard": [0, 1],
                }})
        elif op == 0:  # DISPATCH
            if t == "READY":
                self._session_id = d.get("session_id", "")
                logger.info("QQBot ready, session={}", self._session_id)
            elif t in ("AT_MESSAGE_CREATE", "MESSAGE_CREATE"):
                await self._on_channel_message(d)
            elif t == "GROUP_AT_MESSAGE_CREATE":
                await self._on_group_message(d)
            elif t == "C2C_MESSAGE_CREATE":
                await self._on_c2c_message(d)
        elif op == 7:  # RECONNECT
            if self._ws and not self._ws.closed:
                await self._ws.close()
        elif op == 9:  # INVALID SESSION
            self._session_id = ""
            if self._ws and not self._ws.closed:
                await self._ws.close()
        elif op == 11:  # HEARTBEAT ACK
            pass

    async def _on_channel_message(self, d: dict[str, Any]) -> None:
        author = d.get("author", {})
        if author.get("bot"):
            return
        sender_id = str(author.get("id", ""))
        channel_id = str(d.get("channel_id", ""))
        content = d.get("content", "").strip()
        msg_id = d.get("id", "")
        if not content:
            return
        await self._handle_message(
            sender_id=sender_id, chat_id=channel_id, text=content,
            reply_to_id=msg_id, metadata={"msg_type": "channel", "guild_id": d.get("guild_id", "")},
        )

    async def _on_group_message(self, d: dict[str, Any]) -> None:
        author = d.get("author", {})
        sender_id = str(author.get("member_openid", author.get("id", "")))
        group_id = str(d.get("group_openid", d.get("group_id", "")))
        content = d.get("content", "").strip()
        msg_id = d.get("id", "")
        if not content:
            return
        await self._handle_message(
            sender_id=sender_id, chat_id=group_id, text=content,
            reply_to_id=msg_id, metadata={"msg_type": "group"},
        )

    async def _on_c2c_message(self, d: dict[str, Any]) -> None:
        author = d.get("author", {})
        sender_id = str(author.get("user_openid", author.get("id", "")))
        content = d.get("content", "").strip()
        msg_id = d.get("id", "")
        if not content:
            return
        await self._handle_message(
            sender_id=sender_id, chat_id=sender_id, text=content,
            reply_to_id=msg_id, metadata={"msg_type": "c2c"},
        )

    async def _heartbeat_loop(self) -> None:
        try:
            while self._running and self._ws and not self._ws.closed:
                await self._send_ws({"op": 1, "d": self._seq})
                await asyncio.sleep(self._heartbeat_interval)
        except asyncio.CancelledError:
            pass

    async def _send_ws(self, data: dict[str, Any]) -> None:
        if self._ws and not self._ws.closed:
            await self._ws.send_json(data)

    # ── Auth ─────────────────────────────────────────────────────────────────

    async def _refresh_token(self) -> None:
        if not self._session:
            return
        url = f"{self._api_base}/app/getAppAccessToken"
        payload = {"appId": self._app_id, "clientSecret": self._app_secret}
        try:
            async with self._session.post(url, json=payload) as resp:
                data = await resp.json()
                self._access_token = data.get("access_token", "")
                expires_in = int(data.get("expires_in", 7200))
                self._token_expires = time.time() + expires_in - 300
                logger.info("QQBot access token refreshed")
        except Exception as e:
            logger.error("QQBot token refresh failed: {}", e)

    async def _ensure_token(self) -> None:
        if time.time() >= self._token_expires:
            await self._refresh_token()

    def _auth_headers(self) -> dict[str, str]:
        return {"Authorization": f"QQBot {self._access_token}"}

    async def _get_gateway(self) -> str | None:
        if not self._session:
            return None
        url = f"{self._api_base}/gateway"
        try:
            async with self._session.get(url, headers=self._auth_headers()) as resp:
                data = await resp.json()
                return data.get("url")
        except Exception as e:
            logger.error("QQBot gateway fetch failed: {}", e)
            return None
