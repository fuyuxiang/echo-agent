"""DingTalk channel — Stream Mode WebSocket + HTTP API."""

from __future__ import annotations

import asyncio
import hashlib
import json
import mimetypes
import time
from pathlib import Path
from typing import Any

import aiohttp
from loguru import logger

from echo_agent.bus.events import ContentBlock, ContentType, OutboundEvent
from echo_agent.bus.queue import MessageBus
from echo_agent.channels.base import BaseChannel, SendResult, DeliveryRejected
from echo_agent.config.schema import DingTalkChannelConfig

_API_BASE = "https://api.dingtalk.com"
_OAPI_BASE = "https://oapi.dingtalk.com"
_RECONNECT_BACKOFFS = [2, 5, 10, 30, 60]
_RATE_LIMIT_BACKOFF = 300
# Limits and formats documented by DingTalk's media upload and robot templates.
_MAX_FILE_BYTES = 20 * 1024 * 1024
_FILE_TYPES = frozenset({"xlsx", "pdf", "zip", "rar", "doc", "docx"})
_IMAGE_TYPES = frozenset({"jpg", "gif", "png", "bmp"})
_SEND_TIMEOUT = aiohttp.ClientTimeout(total=30)
_UPLOAD_TIMEOUT = aiohttp.ClientTimeout(total=120)
_BOT_MESSAGE_TOPIC = "/v1.0/im/bot/messages/get"
# Conservative template budget verified against this robot's sampleText API;
# not an assertion that DingTalk measures its advertised limit as raw UTF-8.
_MAX_TEXT_PARAM_BYTES = 15_000
_TEXT_HEADER_RESERVE = 64
_TEXT_SEND_INTERVAL = 3.0


_MessageRejected = DeliveryRejected


def _text_parts(text: str) -> list[str]:
    """Bound escaped template bytes, preferring paragraphs without losing whitespace."""
    def size(value: str) -> int:
        # msgParam is itself a JSON string in the robot request. Count the
        # second escaping layer too, especially quotes, slashes and newlines.
        param = json.dumps({"content": value}, ensure_ascii=False)
        return len(json.dumps(param, ensure_ascii=False).encode("utf-8"))

    if size(text) <= _MAX_TEXT_PARAM_BYTES:
        return [text]
    chunks: list[str] = []
    start = 0
    while start < len(text):
        low, high = start + 1, len(text)
        while low < high:
            middle = (low + high + 1) // 2
            if size(text[start:middle]) <= _MAX_TEXT_PARAM_BYTES - _TEXT_HEADER_RESERVE:
                low = middle
            else:
                high = middle - 1
        end = low
        if end < len(text):
            boundary = text.rfind('\n', start, end)
            if boundary >= start + (end - start) // 2:
                end = boundary + 1
        chunks.append(text[start:end])
        start = end
    return [f'【第{index}/{len(chunks)}段】\n{chunk}' for index, chunk in enumerate(chunks, 1)]


class DingTalkChannel(BaseChannel):
    name = "dingtalk"
    authenticated_ingress = True
    # send_file publishes FILE/IMAGE blocks; both now reach the media API.
    supports_files = True

    def __init__(self, config: DingTalkChannelConfig, bus: MessageBus):
        super().__init__(config, bus)
        self._app_key = config.app_key
        self._app_secret = config.app_secret
        self._robot_code = config.robot_code
        self._session: aiohttp.ClientSession | None = None
        self._access_token: str = ""
        self._token_expires: float = 0
        self._poll_task: asyncio.Task | None = None
        self._rate_limited_until: float = 0
        self._text_send_lock = asyncio.Lock()
        self._last_text_send: float = 0

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
                # stop() initiated the poll cancellation; awaiting it prevents a
                # background task from retaining the HTTP session.
                pass
        if self._session:
            await self._session.close()

    async def send(self, event: OutboundEvent) -> SendResult | None:
        """Validate local attachments, then deliver caption and media in order.

        Fail the entire receipt when any part fails so send_file cannot report
        a caption-only delivery as a successful attachment upload.
        """
        if not self.should_deliver(event):
            return SendResult(success=True, skipped=True)
        text = event.text or ""
        attachments = [b for b in event.content if b.type in (ContentType.FILE, ContentType.IMAGE)]
        if not self._session:
            return SendResult(success=False, error="DingTalk send: no session")
        if not text and not attachments:
            return SendResult(success=False, error="DingTalk send: no text or files")
        message_ids: list[str] = []
        delivery_unknown = False
        try:
            # Preflight every attachment before posting even the caption. Reads
            # are bounded and run off the event loop to keep Stream responsive.
            files = [await asyncio.to_thread(self._read_attachment, block) for block in attachments]
            await self._ensure_token()
            if not self._access_token or time.time() >= self._token_expires:
                raise RuntimeError("DingTalk send: access token refresh failed")
            result = SendResult(success=True)
            if text:
                parts = _text_parts(text)
                logger.debug("DingTalk text: characters={} utf8_bytes={} parts={}",
                             len(text), len(text.encode("utf-8")), len(parts))
                fingerprint = hashlib.sha256(json.dumps(
                    [self._robot_code, event.chat_id, str(event.metadata.get("conversation_type", "")), parts],
                    ensure_ascii=False,
                ).encode()).hexdigest()

                async def before_send(index, part):
                    async with self._text_send_lock:
                        delay = self._last_text_send + _TEXT_SEND_INTERVAL - time.monotonic()
                        if delay > 0:
                            await asyncio.sleep(delay)
                        await self._ensure_token()
                        self._last_text_send = time.monotonic()

                async def send_part(index, part):
                    return await self._send_message(event, "sampleText", {"content": part})

                result = await self.send_parts(event, parts, send_part, fingerprint=fingerprint,
                                               before_send=before_send, require_durable=True,
                                               force_checkpoint=bool(files))
                message_ids.extend(result.message_ids)
                if not result.success:
                    delivery_unknown = result.delivery_unknown
                    # Use the common redaction path for transport/server error messages.
                    raise RuntimeError(result.error)
            async def send_media(index, part):
                name, file_type, media_type, data = files[index]
                try:
                    media_id = await self._upload_media(name, media_type, data)
                except Exception as exc:
                    # An upload alone cannot deliver a robot message; retrying it is safe.
                    raise _MessageRejected(str(exc)) from exc
                if media_type == "image":
                    return await self._send_message(event, "sampleImageMsg", {"photoURL": media_id})
                return await self._send_message(event, "sampleFile", {
                    "mediaId": media_id, "fileName": name, "fileType": file_type,
                })
            if files:
                identities = [json.dumps([name, file_type, media_type, hashlib.sha256(data).hexdigest()])
                              for name, file_type, media_type, data in files]
                media_result = await self.send_parts(event, identities, send_media, namespace="media",
                                                     force_checkpoint=True)
                message_ids.extend(media_result.message_ids)
                if not media_result.success:
                    delivery_unknown = media_result.delivery_unknown
                    raise RuntimeError(media_result.error)
                result = media_result
            result.message_ids = list(message_ids)
            return result
        except (aiohttp.ClientError, asyncio.TimeoutError) as exc:
            # aiohttp exceptions can contain the upload URL's access_token.
            error = f"DingTalk send: network failure ({type(exc).__name__})"
            delivery_unknown = True
        except (OSError, ValueError, RuntimeError) as exc:
            error = str(exc)
        for secret in (self._access_token, self._app_key, self._app_secret):
            if secret:
                error = error.replace(secret, "[redacted]")
        logger.warning("DingTalk send failed: {}", error)
        return SendResult(success=False, error=error, message_ids=message_ids, delivery_unknown=delivery_unknown)

    @staticmethod
    def _read_attachment(block: ContentBlock) -> tuple[str, str, str, bytes]:
        """Read a regular local file within DingTalk's size and format limits."""
        path = Path(block.url)
        if not block.url or not path.exists():
            raise ValueError("DingTalk file not found")
        if not path.is_file():
            raise ValueError("DingTalk attachment must be a regular file")
        name = path.name
        file_type = path.suffix.lstrip(".").lower()
        if file_type == "jpeg":
            file_type = "jpg"
        media_type = "image" if block.type == ContentType.IMAGE else "file"
        allowed_types = _IMAGE_TYPES if media_type == "image" else _FILE_TYPES
        if file_type not in allowed_types:
            raise ValueError(f"DingTalk unsupported file type: {file_type or '(none)'}; use a ZIP archive")
        if path.stat().st_size > _MAX_FILE_BYTES:
            raise ValueError("DingTalk file exceeds 20 MB limit")
        with path.open("rb") as file:
            data = file.read(_MAX_FILE_BYTES + 1)
        if not data:
            raise ValueError("DingTalk file is empty")
        if len(data) > _MAX_FILE_BYTES:
            raise ValueError("DingTalk file exceeds 20 MB limit")
        return name, file_type, media_type, data

    async def _upload_media(self, name: str, media_type: str, data: bytes) -> str:
        """Upload multipart media using the legacy API's query-token auth."""
        form = aiohttp.FormData(quote_fields=False)
        form.add_field("type", media_type)
        form.add_field("media", data, filename=name,
                       content_type=mimetypes.guess_type(name)[0] or "application/octet-stream")
        async with self._session.post(
            f"{_OAPI_BASE}/media/upload", params={"access_token": self._access_token},
            data=form, timeout=_UPLOAD_TIMEOUT,
        ) as resp:
            body = await self._read_api_response(resp, "upload")
        media_id = body.get("media_id")
        if not isinstance(media_id, str) or not media_id:
            raise RuntimeError("DingTalk upload: missing media_id")
        return media_id

    async def _send_message(self, event: OutboundEvent, msg_key: str, msg_param: dict[str, str]) -> SendResult:
        """Use one routing path for text and attachment templates."""
        url = f"{_API_BASE}/v1.0/robot/oToMessages/batchSend"
        payload = {
            "robotCode": self._robot_code,
            "userIds": [event.chat_id],
            "msgKey": msg_key,
            "msgParam": json.dumps(msg_param, ensure_ascii=False),
        }
        # Tool-generated events lack inbound metadata. DingTalk conversation IDs
        # use cid..., while explicit callback metadata remains authoritative.
        conversation_type = str(event.metadata.get("conversation_type", ""))
        if conversation_type == "2" or (not conversation_type and event.chat_id.startswith("cid")):
            url = f"{_API_BASE}/v1.0/robot/groupMessages/send"
            payload.pop("userIds")
            payload["openConversationId"] = event.chat_id
        headers = {"x-acs-dingtalk-access-token": self._access_token, "Content-Type": "application/json"}
        # Match the splitter's UTF-8 encoding instead of aiohttp's default
        # ASCII escaping, which expands Chinese/Emoji and changes the budget.
        body_bytes = json.dumps(payload, ensure_ascii=False).encode("utf-8")
        async with self._session.post(url, data=body_bytes, headers=headers, timeout=_SEND_TIMEOUT) as resp:
            body = await self._read_api_response(resp, "message")
        # batchSend can accept the request yet reject this particular recipient.
        for field in ("invalidStaffIdList", "flowControlledStaffIdList"):
            if body.get(field):
                raise _MessageRejected(f"DingTalk message: recipient rejected ({field})")
        message_id = body.get("processQueryKey")
        if not isinstance(message_id, str) or not message_id:
            raise RuntimeError("DingTalk message: missing processQueryKey")
        return SendResult(success=True, message_id=message_id)

    @staticmethod
    async def _read_api_response(resp: aiohttp.ClientResponse, action: str) -> dict[str, Any]:
        """Reject HTTP and business errors before trusting a delivery receipt."""
        try:
            body = await resp.json()
        except (ValueError, aiohttp.ContentTypeError) as exc:
            if resp.status >= 400:
                error_type = _MessageRejected if action == "message" and 400 <= resp.status < 500 else RuntimeError
                raise error_type(f"DingTalk {action}: HTTP {resp.status}: {(await resp.text())[:200]}") from exc
            raise RuntimeError(f"DingTalk {action}: invalid JSON response") from exc
        if not isinstance(body, dict):
            if action == "message" and 400 <= resp.status < 500:
                raise _MessageRejected(f"DingTalk {action}: HTTP {resp.status}")
            raise RuntimeError(f"DingTalk {action}: invalid JSON object")
        code = body.get("errcode", body.get("code", 0))
        if resp.status >= 400 or code not in (0, "0", None):
            detail = str(body.get("errmsg", body.get("message", "")))[:200]
            error_type = _MessageRejected if action == "message" and (400 <= resp.status < 500 or resp.status < 400) else RuntimeError
            raise error_type(f"DingTalk {action}: HTTP {resp.status}, code={code}: {detail}")
        return body

    async def _callback_register_and_poll(self) -> None:
        backoff_idx = 0
        while self._running:
            wait_until = self._rate_limited_until - time.time()
            if wait_until > 0:
                logger.info("DingTalk rate-limited, waiting {:.0f}s", wait_until)
                await asyncio.sleep(wait_until)
            try:
                await self._ensure_token()
                url = f"{_API_BASE}/v1.0/gateway/connections/open"
                headers = {"x-acs-dingtalk-access-token": self._access_token}
                # Stream authenticates the application and requires an explicit
                # callback subscription to receive messages asking for files.
                payload = {
                    "clientId": self._app_key,
                    "clientSecret": self._app_secret,
                    "subscriptions": [{"type": "CALLBACK", "topic": _BOT_MESSAGE_TOPIC}],
                }
                async with self._session.post(url, json=payload, headers=headers) as resp:
                    if resp.status == 429:
                        self._rate_limited_until = time.time() + _RATE_LIMIT_BACKOFF
                        logger.warning("DingTalk gateway rate-limited (429), backing off {}s", _RATE_LIMIT_BACKOFF)
                        continue
                    if resp.status != 200:
                        logger.error("DingTalk gateway open failed: {}", await resp.text())
                        delay = _RECONNECT_BACKOFFS[min(backoff_idx, len(_RECONNECT_BACKOFFS) - 1)]
                        await asyncio.sleep(delay)
                        backoff_idx += 1
                        continue
                    data = await resp.json()
                    endpoint = data.get("endpoint", "")
                    ticket = data.get("ticket", "")

                if endpoint and ticket:
                    await self._stream_listen(endpoint, ticket)
                    backoff_idx = 0
            except asyncio.CancelledError:
                break
            except Exception as e:
                logger.error("DingTalk stream error: {}", e)
            if self._running:
                delay = _RECONNECT_BACKOFFS[min(backoff_idx, len(_RECONNECT_BACKOFFS) - 1)]
                logger.info("DingTalk reconnecting in {}s", delay)
                await asyncio.sleep(delay)
                backoff_idx += 1

    async def _stream_listen(self, endpoint: str, ticket: str) -> None:
        if not self._session:
            return
        ws_url = f"{endpoint}?ticket={ticket}"
        async with self._session.ws_connect(ws_url) as ws:
            logger.info("DingTalk stream connected")
            callbacks = asyncio.Queue(maxsize=32)

            async def acknowledge(headers, payload, code=200):
                await ws.send_json({"code": code, "headers": {
                    "messageId": headers.get("messageId", ""), "contentType": "application/json",
                }, "message": "OK" if code == 200 else "retry later", "data": payload})

            async def process_callbacks():
                while True:
                    payload, headers = await callbacks.get()
                    try:
                        chat = payload.get("conversationId") if str(payload.get("conversationType")) == "2" else (
                            payload.get("senderStaffId") or payload.get("senderId"))
                        lock = self.bus.delivery_lock(f"ingress:dingtalk:{chat}") if isinstance(self.bus, MessageBus) else asyncio.Lock()
                        async with lock:
                            accepted = await self._on_message(payload, **(
                                {"event_id": headers["messageId"]} if headers.get("messageId") else {}))
                        await acknowledge(headers, json.dumps({"response": "OK"}), 500 if accepted is False else 200)
                    except Exception as exc:
                        logger.warning("DingTalk callback failed ({})", type(exc).__name__)
                        if not getattr(ws, "closed", False):
                            await acknowledge(headers, json.dumps({"response": "retry later"}), 500)
                    finally:
                        callbacks.task_done()

            workers = [asyncio.create_task(process_callbacks()) for _ in range(4)]
            try:
                async for msg in ws:
                    if msg.type == aiohttp.WSMsgType.TEXT:
                        data = json.loads(msg.data)
                        headers = data.get("headers", {})
                        topic = headers.get("topic", "")
                        if topic == _BOT_MESSAGE_TOPIC:
                            try:
                                callbacks.put_nowait((json.loads(data.get("data", "{}")), headers))
                            except (asyncio.QueueFull, ValueError):
                                await acknowledge(headers, json.dumps({"response": "retry later"}), 500)
                            continue
                        ack_data = data.get("data", "{}") if data.get("type") == "SYSTEM" else json.dumps({"response": "OK"})
                        await acknowledge(headers, ack_data)
                        if data.get("type") == "SYSTEM" and topic == "disconnect":
                            break
                    elif msg.type in (aiohttp.WSMsgType.CLOSED, aiohttp.WSMsgType.ERROR):
                        break
                await asyncio.wait_for(callbacks.join(), 30)
            finally:
                for worker in workers:
                    worker.cancel()
                await asyncio.gather(*workers, return_exceptions=True)

    async def _on_message(self, data: dict[str, Any], *, event_id: str | None = None) -> bool:
        sender_id = str(data.get("senderStaffId") or data.get("senderId") or "")
        conversation_type = str(data.get("conversationType", "1"))
        if not sender_id or not self.is_allowed(sender_id):
            return True
        # 私聊与群聊共用入口，接收日志用于区分身份拦截和上游未投递。
        logger.info(
            "DingTalk inbound: conversation_type={} msgtype={} staff_id_present={} sender_allowed={}",
            conversation_type, data.get("msgtype", "text"),
            bool(data.get("senderStaffId")), self.is_allowed(sender_id),
        )
        text = ""
        media: list[dict[str, str]] = []
        msg_type = data.get("msgtype", "text")
        if msg_type == "text":
            text = data.get("text", {}).get("content", "").strip()
        elif msg_type in ('file', 'video'):
            # File bytes stay in the project transport; this channel only exchanges native downloadCode.
            if not self.is_allowed(sender_id):
                return
            content = data.get('content', {})
            name, code = content.get('fileName', ''), content.get('downloadCode', '')
            if msg_type == 'video' and not name:
                from hashlib import sha256
                extension = str(content.get('videoType', '')).lower()
                if extension in ('mp4', 'mov', 'm4v'):
                    name = '钉钉视频-' + sha256(str(data.get('msgId', '')).encode()).hexdigest()[:12] + '.' + extension
            if isinstance(name, str) and isinstance(code, str) and name and code:
                real_url = await self._resolve_download_code(code)
                if real_url:
                    media.append({'type': 'file', 'url': real_url, 'name': name})
                else:
                    return False
        elif msg_type == "picture":
            download_code = data.get("content", {}).get("downloadCode", "")
            if not download_code:
                download_code = data.get("content", {}).get("pictureDownloadCode", "")
            if download_code:
                real_url = await self._resolve_download_code(download_code)
                if real_url:
                    media.append({"type": "image", "url": real_url})
                else:
                    logger.warning("DingTalk picture download code resolution failed")
                    return False
        elif msg_type == "richText":
            for item in data.get("content", {}).get("richText", []):
                if "text" in item:
                    text += item["text"]
                if "downloadCode" in item:
                    real_url = await self._resolve_download_code(item["downloadCode"])
                    if real_url:
                        media.append({"type": "image", "url": real_url})
                    else:
                        return False
        if not text and not media:
            return True
        chat_id = sender_id if conversation_type == "1" else data.get("conversationId", "")
        event = await self._handle_message(
            sender_id=sender_id, chat_id=chat_id, text=text,
            media=media or None,
            metadata={"conversation_type": conversation_type},
            is_group=conversation_type == "2",
            **({'event_id': data.get('msgId') or event_id} if data.get('msgId') or event_id else {}),
        )
        return event is not None

    async def _resolve_download_code(self, download_code: str) -> str | None:
        """Convert DingTalk downloadCode to a real download URL.

        DingTalk's downloadCode is a temporary token that needs to be exchanged
        for an actual download URL via the API.
        """
        if not self._session or not download_code:
            return None
        await self._ensure_token()
        # Use the media download API to get the real URL
        url = f"{_API_BASE}/v1.0/robot/messageFiles/download"
        headers = {"x-acs-dingtalk-access-token": self._access_token}
        payload = {"downloadCode": download_code, "robotCode": self._robot_code}
        try:
            async with self._session.post(url, json=payload, headers=headers, timeout=_SEND_TIMEOUT) as resp:
                if resp.status == 200:
                    data = await resp.json()
                    return data.get("downloadUrl", "")
                else:
                    logger.warning("DingTalk download code resolution failed HTTP={}", resp.status)
        except Exception as e:
            logger.error("DingTalk download code resolution error exception={}", type(e).__name__)
        return None

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
