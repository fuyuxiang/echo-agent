"""Telegram channel — Bot API with long polling."""

from __future__ import annotations

import asyncio
import json
import os
import re
import time
from pathlib import Path
from typing import Any

import aiohttp
from loguru import logger

from echo_agent.bus.events import OutboundEvent
from echo_agent.bus.queue import MessageBus
from echo_agent.channels.base import BaseChannel, SendResult
from echo_agent.config.schema import TelegramChannelConfig
from echo_agent.utils.text import split_message
from echo_agent.bus.events import PollRequest

_API = "https://api.telegram.org/bot{token}/{method}"
_MAX_TEXT = 4096

# Telegram's `parse_mode=HTML` rejects `<`, `>` and `&` that aren't part of
# a known tag. The agent's text passes straight through with whatever the
# LLM wrote — code with generics (`Foo<T>`), shell redirections (`> /dev/null`),
# comparisons (`a & b`), and ampersands all show up in real replies and each
# one makes Telegram return 400, dropping the whole message. Escape the three
# metacharacters so the reply always lands. Callers that genuinely want
# bold/italic markup can mark the event with metadata["telegram_markup"]
# = True to opt out.
_TG_HTML_UNSAFE_RE = re.compile(r"[<>&]")
_TG_HTML_ESCAPE = {"<": "&lt;", ">": "&gt;", "&": "&amp;"}


def _escape_html(text: str) -> str:
    return _TG_HTML_UNSAFE_RE.sub(lambda m: _TG_HTML_ESCAPE[m.group(0)], text)
_RECONNECT_BACKOFFS = [2, 5, 10, 30, 60]


def _offset_path(data_dir: Path, bot_id: str) -> Path:
    return data_dir / f"{bot_id}.offset.json"


def _load_offset(data_dir: Path, bot_id: str) -> int:
    """Read the persisted long-poll offset for this bot.

    Returns 0 for any non-happy path — no saved state, unreadable file, or
    structurally wrong JSON (e.g. a top-level list instead of an object). The
    file is keyed by ``bot_id`` (the filename), so a different bot naturally
    reads a different file and a token rotation on the *same* bot keeps its
    offset; there is no token check to get wrong."""
    path = _offset_path(data_dir, bot_id)
    if not path.exists():
        return 0
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
        return int(data["offset"])
    except (OSError, ValueError, TypeError, KeyError) as e:
        logger.debug("telegram: failed to load offset, starting from 0: {}", e)
        return 0


def _save_offset(data_dir: Path, bot_id: str, offset: int) -> None:
    """Persist the offset atomically: write to a temp file then ``os.replace``
    so a crash mid-write leaves the previous good file intact rather than a
    truncated one."""
    path = _offset_path(data_dir, bot_id)
    tmp = path.with_name(f"{path.name}.tmp")
    try:
        tmp.write_text(json.dumps({"offset": offset}), encoding="utf-8")
        os.replace(tmp, path)
    except OSError as e:
        logger.warning("telegram: failed to save offset: {}", e)


class TelegramChannel(BaseChannel):
    name = "telegram"
    supports_edit = True
    supports_reactions = True

    def __init__(self, config: TelegramChannelConfig, bus: MessageBus):
        super().__init__(config, bus)
        self._token = config.token
        self._session: aiohttp.ClientSession | None = None
        self._poll_task: asyncio.Task | None = None
        self._offset = 0
        self._rate_limited_until: float = 0
        self._group_policy = config.group_policy
        self._bot_id: str = ""
        self._bot_username: str = ""
        data_dir = config.data_dir or os.path.expanduser("~/.echo-agent/data/telegram")
        self._data_dir = Path(data_dir)

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
        # Restore the persisted long-poll offset so a restart does not re-pull
        # (and re-answer) updates the previous process already acknowledged. The
        # offset file is keyed by bot_id, so if getMe failed (network hiccup) we
        # have no key: skip persistence for this process and start from 0 —
        # Telegram will re-deliver the last unacked window, favouring re-answer
        # over silent loss.
        if self._bot_id:
            try:
                self._data_dir.mkdir(parents=True, exist_ok=True)
            except OSError as e:
                logger.warning("telegram: cannot create offset dir {}: {}", self._data_dir, e)
            self._offset = _load_offset(self._data_dir, self._bot_id)
        else:
            logger.warning("telegram: getMe failed, offset persistence disabled this run")
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

    async def send(self, event: OutboundEvent) -> SendResult | None:
        text = event.text or ""
        if not text:
            return None
        # Default: escape HTML metacharacters so any character the LLM emits
        # lands. Set metadata["telegram_markup"] = True to send pre-formatted
        # HTML and skip escaping.
        if not event.metadata.get("telegram_markup"):
            text = _escape_html(text)
        chat_id = event.chat_id
        reply_to = event.reply_to_id
        first_result: SendResult | None = None
        for chunk in self._chunk_text(text, _MAX_TEXT):
            payload = {
                "chat_id": chat_id,
                "text": chunk,
                "parse_mode": "HTML",
                **({"reply_to_message_id": reply_to} if reply_to else {}),
            }
            if reply_to:
                # 被引用消息可能已被删除，Telegram 会报 "message to be replied not
                # found"。这种情况下去掉锚点重发一次，避免整条回复发不出去。
                result, err = await self._api("sendMessage", _return_error=True, json=payload)
                if result is None and "replied" in err.lower():
                    logger.debug("Telegram reply anchor gone, resending without it: {}", err)
                    payload.pop("reply_to_message_id", None)
                    result = await self._api("sendMessage", json=payload)
            else:
                result = await self._api("sendMessage", json=payload)
            send_result = self._send_result(result, "Telegram sendMessage failed")
            if first_result is None:
                first_result = send_result
            reply_to = None
        return first_result

    async def edit_message(
        self,
        chat_id: str,
        message_id: str,
        text: str,
        *,
        metadata: dict[str, Any] | None = None,
        finalize: bool = False,
    ) -> SendResult:
        if not text:
            return SendResult(success=False, message_id=message_id, error="empty text")
        # Mirror send(): escape HTML unless the caller marked the text as
        # pre-formatted Telegram markup.
        if not (metadata or {}).get("telegram_markup"):
            text = _escape_html(text)
        result = await self._api("editMessageText", json={
            "chat_id": chat_id,
            "message_id": message_id,
            "text": text,
            "parse_mode": "HTML",
        })
        return self._send_result(result, "Telegram editMessageText failed", fallback_message_id=message_id)

    async def send_typing(self, chat_id: str, metadata: dict[str, Any] | None = None) -> None:
        await self._api("sendChatAction", json={"chat_id": chat_id, "action": "typing"})

    async def send_reaction(self, chat_id: str, message_id: str, emoji: str, metadata: dict[str, Any] | None = None) -> SendResult:
        if not getattr(self.config, "reactions_enabled", True):
            return SendResult(success=False, error="reactions disabled")
        result = await self._api("setMessageReaction", json={
            "chat_id": chat_id,
            "message_id": message_id,
            "reaction": [{"type": "emoji", "emoji": emoji}],
        })
        if result is not None:
            return SendResult(success=True)
        return SendResult(success=False, error="Telegram setMessageReaction failed")

    async def remove_reaction(self, chat_id: str, message_id: str, emoji: str, metadata: dict[str, Any] | None = None) -> SendResult:
        if not getattr(self.config, "reactions_enabled", True):
            return SendResult(success=False, error="reactions disabled")
        result = await self._api("setMessageReaction", json={
            "chat_id": chat_id,
            "message_id": message_id,
            "reaction": [],
        })
        if result is not None:
            return SendResult(success=True)
        return SendResult(success=False, error="Telegram remove reaction failed")

    async def send_poll(self, chat_id: str, poll: PollRequest, metadata: dict[str, Any] | None = None) -> SendResult:
        payload: dict[str, Any] = {
            "chat_id": chat_id,
            "question": poll.question,
            "options": [{"text": o} for o in poll.options],
            "is_anonymous": False,
            "allows_multiple_answers": poll.allow_multiple,
        }
        if poll.duration_seconds and 5 <= poll.duration_seconds <= 600:
            payload["open_period"] = poll.duration_seconds
        result = await self._api("sendPoll", json=payload)
        return self._send_result(result, "Telegram sendPoll failed")

    async def delete_message(self, chat_id: str, message_id: str, metadata: dict[str, Any] | None = None) -> SendResult:
        result = await self._api("deleteMessage", json={
            "chat_id": chat_id,
            "message_id": message_id,
        })
        if result is not None:
            return SendResult(success=True)
        return SendResult(success=False, error="Telegram deleteMessage failed")

    async def send_voice(self, chat_id: str, audio_source: str, metadata: dict[str, Any] | None = None) -> SendResult:
        result = await self._api("sendVoice", json={
            "chat_id": chat_id,
            "voice": audio_source,
        })
        return self._send_result(result, "Telegram sendVoice failed")

    async def _poll_loop(self) -> None:
        backoff_idx = 0
        while self._running:
            wait_until = self._rate_limited_until - time.time()
            if wait_until > 0:
                logger.info("Telegram rate-limited, waiting {:.0f}s", wait_until)
                await asyncio.sleep(wait_until)
            try:
                updates = await self._api("getUpdates", json={
                    "offset": self._offset,
                    "timeout": 30,
                    "allowed_updates": ["message"],
                })
                if not updates:
                    delay = _RECONNECT_BACKOFFS[min(backoff_idx, len(_RECONNECT_BACKOFFS) - 1)]
                    await asyncio.sleep(delay)
                    backoff_idx += 1
                    continue
                backoff_idx = 0
                for update in updates:
                    self._offset = update["update_id"] + 1
                    await self._process_update(update)
                # Persist once per batch (not per update) to bound disk I/O;
                # the offset advances even if a single update fails to process,
                # matching Telegram's long-poll semantics (fetching offset N acks
                # everything below it upstream).
                if self._bot_id:
                    _save_offset(self._data_dir, self._bot_id, self._offset)
            except asyncio.CancelledError:
                break
            except Exception as e:
                logger.error("Telegram poll error: {}", e)
                delay = _RECONNECT_BACKOFFS[min(backoff_idx, len(_RECONNECT_BACKOFFS) - 1)]
                await asyncio.sleep(delay)
                backoff_idx += 1

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
                if not file_id:
                    continue
                media_type = "image" if kind == "photo" else kind
                local_path = await self._download_telegram_file(file_id)
                if local_path:
                    media.append({"type": media_type, "url": local_path})
                else:
                    logger.warning("Telegram file download failed, skipping: {}", file_id[:30])

        if not text and not media:
            return

        await self._api("sendChatAction", json={"chat_id": chat_id, "action": "typing"})

        # 引用上下文：用户回复某条消息时 Telegram 在 reply_to_message 带上被引用消息，
        # 解析其原文/作者交给 pipeline 统一注入（reply_to_id 仍是本条消息的锚点）。
        reply_to_text = reply_to_sender = None
        reply_to_is_own = False
        replied = msg.get("reply_to_message") or {}
        if replied:
            reply_to_text = replied.get("text") or replied.get("caption") or None
            replied_from = replied.get("from", {})
            reply_to_sender = (
                replied_from.get("first_name")
                or replied_from.get("username")
                or str(replied_from.get("id", "")) or None
            )
            reply_to_is_own = bool(self._bot_id) and str(replied_from.get("id", "")) == self._bot_id

        await self._handle_message(
            sender_id=sender_id,
            chat_id=chat_id,
            text=text,
            media=media if media else None,
            reply_to_id=str(msg.get("message_id", "")),
            reply_to_text=reply_to_text,
            reply_to_sender=reply_to_sender,
            reply_to_is_own=reply_to_is_own,
            metadata={"chat_type": chat.get("type", "private")},
            is_group=chat.get("type") in ("group", "supergroup"),
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

    async def _download_telegram_file(self, file_id: str) -> str | None:
        """Resolve a Telegram file_id to a local cached path via getFile + download."""
        async def fetch() -> bytes:
            result = await self._api("getFile", json={"file_id": file_id})
            if not result:
                raise RuntimeError("getFile returned no result")
            file_path = result.get("file_path", "")
            if not file_path:
                raise RuntimeError("getFile returned empty file_path")
            download_url = f"https://api.telegram.org/file/bot{self._token}/{file_path}"
            if not self._session:
                raise RuntimeError("no session")
            async with self._session.get(download_url) as resp:
                if resp.status != 200:
                    raise RuntimeError(f"download failed ({resp.status})")
                return await resp.read()

        ext = ".jpg"
        return await self._resolve_media_to_cache(file_id, "telegram", fetch, suffix=ext)

    async def _api(self, method: str, *, _return_error: bool = False, **kwargs: Any) -> Any:
        if not self._session:
            return (None, "") if _return_error else None
        url = _API.format(token=self._token, method=method)
        try:
            async with self._session.post(url, **kwargs) as resp:
                data = await resp.json()
                if not data.get("ok"):
                    if data.get("error_code") == 429:
                        retry_after = data.get("parameters", {}).get("retry_after", 300)
                        self._rate_limited_until = time.time() + retry_after
                        logger.warning("Telegram rate-limited, retry_after={}s", retry_after)
                    else:
                        logger.warning("Telegram API {}: {}", method, data.get("description", ""))
                    return (None, str(data.get("description", ""))) if _return_error else None
                result = data.get("result")
                return (result, "") if _return_error else result
        except Exception as e:
            logger.error("Telegram API {} failed: {}", method, e)
            return (None, str(e)) if _return_error else None

    @staticmethod
    def _send_result(
        result: Any,
        error: str,
        *,
        fallback_message_id: str = "",
    ) -> SendResult:
        if isinstance(result, dict):
            message_id = str(result.get("message_id") or fallback_message_id)
            return SendResult(success=True, message_id=message_id)
        return SendResult(success=False, message_id=fallback_message_id, error=error)

    @staticmethod
    def _chunk_text(text: str, limit: int) -> list[str]:
        return split_message(text, limit)
