"""DingTalk channel — Stream Mode WebSocket + HTTP API."""

from __future__ import annotations

import asyncio
import json
import time
import uuid
from typing import Any

import aiohttp
from loguru import logger

from echo_agent.bus.events import OutboundEvent
from echo_agent.bus.queue import MessageBus
from echo_agent.channels.base import BaseChannel
from echo_agent.config.schema import DingTalkChannelConfig

_API_BASE = "https://api.dingtalk.com"
_OAPI_BASE = "https://oapi.dingtalk.com"


class DingTalkChannel(BaseChannel):
    name = "dingtalk"

    def __init__(self, config: DingTalkChannelConfig, bus: MessageBus):
        super().__init__(config, bus)
        self._app_key = config.app_key
        self._app_secret = config.app_secret
        self._robot_code = config.robot_code
        self._session: aiohttp.ClientSession | None = None
        self._access_token: str = ""
        self._token_expires: float = 0
        self._poll_task: asyncio.Task | None = None

    async def start(self) -> None:
        self._session = aiohttp.ClientSession()
        await self._refresh_token()
        self._running = True
        self.bus.subscribe_outbound(self.name, self.send)
        self._poll_task = asyncio.create_task(self._callback_register_and_poll())
        logger.info("DingTalk channel started")

    async def stop(self) -> None:
        self._running = False
        if self._poll_task:
            self._poll_task.cancel()
            try:
                await self._poll_task
            except asyncio.CancelledError:
                pass
        if self._session:
            await self._session.close()

    async def send(self, event: OutboundEvent) -> None:
        text = event.text or ""
        if not text or not self._session:
            return
        await self._ensure_token()
        url = f"{_API_BASE}/v1.0/robot/oToMessages/batchSend"
        payload = {
            "robotCode": self._robot_code,
            "userIds": [event.chat_id],
            "msgKey": "sampleText",
            "msgParam": json.dumps({"content": text}),
        }
        if event.metadata.get("conversation_type") == "2":
            url = f"{_API_BASE}/v1.0/robot/groupMessages/send"
            payload = {
                "robotCode": self._robot_code,
                "openConversationId": event.chat_id,
                "msgKey": "sampleText",
                "msgParam": json.dumps({"content": text}),
            }
        headers = {"x-acs-dingtalk-access-token": self._access_token}
        try:
            async with self._session.post(url, json=payload, headers=headers) as resp:
                if resp.status >= 400:
                    body = await resp.text()
                    logger.warning("DingTalk send failed ({}): {}", resp.status, body[:200])
        except Exception as e:
            logger.error("DingTalk send error: {}", e)

    async def _callback_register_and_poll(self) -> None:
        while self._running:
            try:
                await self._ensure_token()
                url = f"{_API_BASE}/v1.0/gateway/connections/open"
                headers = {"x-acs-dingtalk-access-token": self._access_token}
                payload = {"clientId": uuid.uuid4().hex, "clientSecret": self._app_secret}
                async with self._session.post(url, json=payload, headers=headers) as resp:
                    if resp.status != 200:
                        logger.error("DingTalk gateway open failed: {}", await resp.text())
                        await asyncio.sleep(10)
                        continue
                    data = await resp.json()
                    endpoint = data.get("endpoint", "")
                    ticket = data.get("ticket", "")

                if endpoint and ticket:
                    await self._stream_listen(endpoint, ticket)
            except asyncio.CancelledError:
                break
            except Exception as e:
                logger.error("DingTalk stream error: {}", e)
            if self._running:
                await asyncio.sleep(5)

    async def _stream_listen(self, endpoint: str, ticket: str) -> None:
        if not self._session:
            return
        ws_url = f"{endpoint}?ticket={ticket}"
        async with self._session.ws_connect(ws_url) as ws:
            logger.info("DingTalk stream connected")
            async for msg in ws:
                if msg.type == aiohttp.WSMsgType.TEXT:
                    data = json.loads(msg.data)
                    headers = data.get("headers", {})
                    topic = headers.get("topic", "")
                    if topic == "/v1.0/im/bot/messages/get":
                        payload = json.loads(data.get("data", "{}"))
                        await self._on_message(payload)
                    await ws.send_json({"code": 200, "headers": headers, "message": "OK", "data": ""})
                elif msg.type in (aiohttp.WSMsgType.CLOSED, aiohttp.WSMsgType.ERROR):
                    break

    async def _on_message(self, data: dict[str, Any]) -> None:
        sender_id = data.get("senderStaffId", data.get("senderId", ""))
        conversation_type = data.get("conversationType", "1")
        text = ""
        media: list[dict[str, str]] = []
        msg_type = data.get("msgtype", "text")
        if msg_type == "text":
            text = data.get("text", {}).get("content", "").strip()
        elif msg_type == "picture":
            pic_url = data.get("content", {}).get("downloadCode", "")
            if not pic_url:
                pic_url = data.get("content", {}).get("pictureDownloadCode", "")
            if pic_url:
                media.append({"type": "image", "url": pic_url})
        elif msg_type == "richText":
            for item in data.get("content", {}).get("richText", []):
                if "text" in item:
                    text += item["text"]
                if "downloadCode" in item:
                    media.append({"type": "image", "url": item["downloadCode"]})
        if not text and not media:
            return
        chat_id = sender_id if conversation_type == "1" else data.get("conversationId", "")
        await self._handle_message(
            sender_id=sender_id, chat_id=chat_id, text=text,
            media=media or None,
            metadata={"conversation_type": conversation_type},
        )

    async def _refresh_token(self) -> None:
        if not self._session:
            return
        url = f"{_OAPI_BASE}/gettoken?appkey={self._app_key}&appsecret={self._app_secret}"
        try:
            async with self._session.get(url) as resp:
                data = await resp.json()
                if data.get("errcode") == 0:
                    self._access_token = data.get("access_token", "")
                    self._token_expires = time.time() + data.get("expires_in", 7200) - 300
                    logger.info("DingTalk token refreshed")
                else:
                    logger.error("DingTalk token error: {}", data)
        except Exception as e:
            logger.error("DingTalk token refresh failed: {}", e)

    async def _ensure_token(self) -> None:
        if time.time() >= self._token_expires:
            await self._refresh_token()
