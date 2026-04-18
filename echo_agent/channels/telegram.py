"""Telegram channel — Bot API with long polling."""

from __future__ import annotations

import asyncio
from typing import Any

import aiohttp
from loguru import logger

from echo_agent.bus.events import OutboundEvent
from echo_agent.bus.queue import MessageBus
from echo_agent.channels.base import BaseChannel
from echo_agent.config.schema import TelegramChannelConfig

_API = "https://api.telegram.org/bot{token}/{method}"
_MAX_TEXT = 4096


class TelegramChannel(BaseChannel):
    name = "telegram"

    def __init__(self, config: TelegramChannelConfig, bus: MessageBus):
        super().__init__(config, bus)
        self._token = config.token
        self._session: aiohttp.ClientSession | None = None
        self._poll_task: asyncio.Task | None = None
        self._offset = 0
        self._group_policy = config.group_policy
        self._bot_id: str = ""
        self._bot_username: str = ""

    async def start(self) -> None:
        connector = None
        if self.config.proxy:
            from aiohttp_socks import ProxyConnector
            connector = ProxyConnector.from_url(self.config.proxy)
        self._session = aiohttp.ClientSession(connector=connector)
        me = await self._api("getMe")
        if me:
            self._bot_id = str(me.get("id", ""))
            self._bot_username = me.get("username", "")
            logger.info("Telegram bot: @{}", self._bot_username)
        self._running = True
        self.bus.subscribe_outbound(self.name, self.send)
        self._poll_task = asyncio.create_task(self._poll_loop())
        logger.info("Telegram channel started")

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
        if not text:
            return
        chat_id = event.chat_id
        reply_to = event.reply_to_id
        for chunk in self._chunk_text(text, _MAX_TEXT):
            await self._api("sendMessage", json={
                "chat_id": chat_id,
                "text": chunk,
                "parse_mode": "HTML",
                **({"reply_to_message_id": reply_to} if reply_to else {}),
            })
            reply_to = None

    async def _poll_loop(self) -> None:
        while self._running:
            try:
                updates = await self._api("getUpdates", json={
                    "offset": self._offset,
                    "timeout": 30,
                    "allowed_updates": ["message"],
                })
                if not updates:
                    continue
                for update in updates:
                    self._offset = update["update_id"] + 1
                    await self._process_update(update)
            except asyncio.CancelledError:
                break
            except Exception as e:
                logger.error("Telegram poll error: {}", e)
                await asyncio.sleep(5)

    async def _process_update(self, update: dict[str, Any]) -> None:
        msg = update.get("message")
        if not msg:
            return
        chat = msg.get("chat", {})
        chat_id = str(chat.get("id", ""))
        sender = msg.get("from", {})
        sender_id = str(sender.get("id", ""))
        text = msg.get("text", "") or msg.get("caption", "") or ""

        if chat.get("type") in ("group", "supergroup") and self._group_policy == "mention":
            if not self._is_mentioned(msg, text):
                return

        media: list[dict[str, str]] = []
        for kind in ("photo", "document", "audio", "video", "voice"):
            if kind in msg:
                file_obj = msg[kind][-1] if kind == "photo" else msg[kind]
                file_id = file_obj.get("file_id", "")
                if file_id:
                    media.append({"type": "image" if kind == "photo" else kind, "url": file_id})

        if not text and not media:
            return

        await self._api("sendChatAction", json={"chat_id": chat_id, "action": "typing"})

        await self._handle_message(
            sender_id=sender_id,
            chat_id=chat_id,
            text=text,
            media=media if media else None,
            reply_to_id=str(msg.get("message_id", "")),
            metadata={"chat_type": chat.get("type", "private")},
        )

    def _is_mentioned(self, msg: dict[str, Any], text: str) -> bool:
        if self._bot_username and f"@{self._bot_username}" in text:
            return True
        entities = msg.get("entities", [])
        for ent in entities:
            if ent.get("type") == "mention":
                mention = text[ent["offset"]:ent["offset"] + ent["length"]]
                if mention.lower() == f"@{self._bot_username.lower()}":
                    return True
        reply = msg.get("reply_to_message", {})
        if reply and str(reply.get("from", {}).get("id", "")) == self._bot_id:
            return True
        return False

    async def _api(self, method: str, **kwargs: Any) -> Any:
        if not self._session:
            return None
        url = _API.format(token=self._token, method=method)
        try:
            async with self._session.post(url, **kwargs) as resp:
                data = await resp.json()
                if not data.get("ok"):
                    logger.warning("Telegram API {}: {}", method, data.get("description", ""))
                    return None
                return data.get("result")
        except Exception as e:
            logger.error("Telegram API {} failed: {}", method, e)
            return None

    @staticmethod
    def _chunk_text(text: str, limit: int) -> list[str]:
        if len(text) <= limit:
            return [text]
        chunks: list[str] = []
        while text:
            if len(text) <= limit:
                chunks.append(text)
                break
            cut = text.rfind("\n", 0, limit)
            if cut <= 0:
                cut = limit
            chunks.append(text[:cut])
            text = text[cut:].lstrip("\n")
        return chunks
