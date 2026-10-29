"""Weixin (personal WeChat) channel — iLink Bot API with long-polling.

Connects to personal WeChat accounts via Tencent's iLink Bot API.
No public endpoint required; uses HTTP long-polling for inbound messages.
"""

from __future__ import annotations

import asyncio
import base64
import hashlib
import json
import mimetypes
import os
import secrets
import struct
import tempfile
import time
import uuid
from pathlib import Path
from typing import Any

import aiohttp
from loguru import logger

from echo_agent.bus.events import ContentType, OutboundEvent
from echo_agent.bus.queue import MessageBus
from echo_agent.channels.base import BaseChannel, SendResult
from echo_agent.config.schema import WeixinChannelConfig
from echo_agent.media.silk import encode_to_silk
from echo_agent.utils.text import split_message

try:
    from cryptography.hazmat.backends import default_backend
    from cryptography.hazmat.primitives.ciphers import Cipher, algorithms, modes
    _CRYPTO_AVAILABLE = True
except ImportError:
    _CRYPTO_AVAILABLE = False

# ── iLink API constants ─────────────────────────────────────────────────────

_CHANNEL_VERSION = "2.2.0"
_APP_ID = "bot"
_APP_CLIENT_VERSION = (2 << 16) | (2 << 8) | 0

_EP_GET_UPDATES = "ilink/bot/getupdates"
_EP_SEND_MESSAGE = "ilink/bot/sendmessage"
_EP_GET_UPLOAD_URL = "ilink/bot/getuploadurl"
_EP_GET_BOT_QR = "ilink/bot/get_bot_qrcode"
_EP_GET_QR_STATUS = "ilink/bot/get_qrcode_status"

_LONG_POLL_TIMEOUT_MS = 35_000
_API_TIMEOUT_MS = 15_000
_MAX_CONSECUTIVE_FAILURES = 3
_RETRY_DELAY = 2
_BACKOFF_DELAY = 30
_SESSION_EXPIRED_ERRCODE = -14
_DEDUP_TTL = 300
_MAX_MESSAGE_LENGTH = 4000
_LIVENESS_TIMEOUT = 30 * 60  # 30 分钟无消息视为静默失效

_ITEM_TEXT = 1
_ITEM_IMAGE = 2
_ITEM_VOICE = 3
_ITEM_FILE = 4
_ITEM_VIDEO = 5
_MSG_TYPE_BOT = 2
_MSG_STATE_FINISH = 2

# media_type values for getuploadurl (distinct from item types above)
_MEDIA_IMAGE = 1
_MEDIA_VIDEO = 2
_MEDIA_FILE = 3
_MEDIA_VOICE = 4


# ── Helpers ──────────────────────────────────────────────────────────────────

def _random_uin() -> str:
    value = struct.unpack(">I", secrets.token_bytes(4))[0]
    return base64.b64encode(str(value).encode()).decode("ascii")


def _json_dumps(payload: dict[str, Any]) -> str:
    return json.dumps(payload, ensure_ascii=False, separators=(",", ":"))


def _headers(token: str | None, body: str) -> dict[str, str]:
    h = {
        "Content-Type": "application/json",
        "AuthorizationType": "ilink_bot_token",
        "Content-Length": str(len(body.encode())),
        "X-WECHAT-UIN": _random_uin(),
        "iLink-App-Id": _APP_ID,
        "iLink-App-ClientVersion": str(_APP_CLIENT_VERSION),
    }
    if token:
        h["Authorization"] = f"Bearer {token}"
    return h


def _login_headers() -> dict[str, str]:
    return {
        "X-WECHAT-UIN": _random_uin(),
        "iLink-App-Id": _APP_ID,
        "iLink-App-ClientVersion": str(_APP_CLIENT_VERSION),
    }


def _pkcs7_pad(data: bytes, block_size: int = 16) -> bytes:
    pad_len = block_size - (len(data) % block_size)
    return data + bytes([pad_len] * pad_len)


def _aes128_ecb_decrypt(ciphertext: bytes, key: bytes) -> bytes:
    if not _CRYPTO_AVAILABLE:
        raise RuntimeError("cryptography package required for media decryption")
    cipher = Cipher(algorithms.AES(key), modes.ECB(), backend=default_backend())
    decryptor = cipher.decryptor()
    padded = decryptor.update(ciphertext) + decryptor.finalize()
    if not padded:
        return padded
    pad_len = padded[-1]
    if 1 <= pad_len <= 16 and padded.endswith(bytes([pad_len]) * pad_len):
        return padded[:-pad_len]
    return padded


def _aes128_ecb_encrypt(plaintext: bytes, key: bytes) -> bytes:
    if not _CRYPTO_AVAILABLE:
        raise RuntimeError("cryptography package required for media encryption")
    cipher = Cipher(algorithms.AES(key), modes.ECB(), backend=default_backend())
    encryptor = cipher.encryptor()
    return encryptor.update(_pkcs7_pad(plaintext)) + encryptor.finalize()


def _aes_padded_size(size: int) -> int:
    """Ciphertext length after PKCS#7 padding to the next 16-byte boundary."""
    return ((size + 1 + 15) // 16) * 16


def _parse_aes_key(aes_key_b64: str) -> bytes:
    decoded = base64.b64decode(aes_key_b64)
    if len(decoded) == 16:
        return decoded
    if len(decoded) == 32:
        text = decoded.decode("ascii", errors="ignore")
        if all(ch in "0123456789abcdefABCDEF" for ch in text):
            return bytes.fromhex(text)
    raise ValueError(f"unexpected aes_key format ({len(decoded)} decoded bytes)")


# ── API helpers ──────────────────────────────────────────────────────────────

def _api_error(response: dict[str, Any]) -> str:
    """本次调用的失败原因,成功则返回空串。

    iLink 把失败分放在两个字段里:``errcode`` 和 ``ret``。轮询循环一直是两个
    都查的(见 _poll_loop),发送路径过去只查 ``errcode`` 且缺省为 0,于是一个
    形如 ``{"ret": -14}`` 的会话过期响应在轮询侧是致命错误,在发送侧却被读成
    发送成功 —— 这正是"日志全绿但微信没收到"的成因之一。判定收敛到这里,
    两条路径从此对"什么算失败"给出同一个答案。

    ``ret``/``errcode`` 缺失当成 0(成功):这两个字段在正常响应里本就可以不
    出现,把缺失当失败会把每一次正常发送都判成错。真正的空响应由调用方的
    HTTP 状态检查与 _require_ok 的空 body 分支拦住。
    """
    for field in ("errcode", "ret"):
        raw = response.get(field)
        if raw in (None, 0):
            continue
        try:
            code = int(raw)
        except (TypeError, ValueError):
            return f"{field}={raw!r}"
        if code == 0:
            continue
        errmsg = str(response.get("errmsg") or "")
        if code == _SESSION_EXPIRED_ERRCODE:
            # 会话过期在发送侧无法自愈:token 已经不再有投递权,重试只会
            # 继续静默失败。把语义点明,运维看日志就知道要重新扫码登录。
            suffix = f": {errmsg}" if errmsg else ""
            return f"{field}={code} (session expired, re-login required){suffix}"
        return f"{field}={code}: {errmsg}"
    return ""


def _is_session_expired(response: dict[str, Any]) -> bool:
    """响应是否表示 bot 会话已过期(两个字段任一为 -14)。"""
    return any(
        response.get(field) == _SESSION_EXPIRED_ERRCODE for field in ("errcode", "ret")
    )


def _require_ok(response: dict[str, Any], *, what: str) -> str:
    """把"不像一个成功响应"的 body 也算成失败,返回失败原因或空串。

    除了显式错误码,还有一类静默失败:响应根本不是 iLink 的 JSON 结构(网关
    插的错误页、代理返回的空 body)。这种 body 里没有 errcode,过去被 dict.get
    的缺省值读成成功。
    """
    if not isinstance(response, dict) or not response:
        return f"{what} returned an empty or non-JSON response"
    return _api_error(response)


def _server_message_id(response: dict[str, Any]) -> str:
    """Read the uint64 server receipt, including message-id-only responses."""
    raw = response.get("message_id")
    if isinstance(raw, bool) or not isinstance(raw, (str, int)):
        return ""
    if isinstance(raw, int) and not 0 < raw < 2**64:
        return ""
    value = str(raw).strip()
    # Bound the conversion as well as the value: arbitrarily long decimal
    # strings can raise ValueError under Python's integer conversion limit.
    digits = value.lstrip("0")
    if not digits.isascii() or not digits.isdecimal() or len(digits) > 20:
        return ""
    return value if 0 < int(digits) < 2**64 else ""


async def _api_post(
    session: aiohttp.ClientSession,
    *,
    base_url: str,
    endpoint: str,
    payload: dict[str, Any],
    token: str,
    timeout_ms: int,
) -> dict[str, Any]:
    body = _json_dumps({"base_info": {"channel_version": _CHANNEL_VERSION}, **payload})
    url = f"{base_url}/{endpoint}"
    timeout = aiohttp.ClientTimeout(total=timeout_ms / 1000 + 5)
    async with session.post(url, data=body.encode(), headers=_headers(token, body), timeout=timeout) as resp:
        # HTTP 状态先于 body 判定:非 2xx 时 body 往往是网关的错误页而不是
        # iLink 的 JSON,解析出来没有 errcode,会被下游读成发送成功。CDN 上传
        # 那一步一直是查状态码的,这里补齐同样的检查。
        if resp.status < 200 or resp.status >= 300:
            raw = await resp.text()
            raise RuntimeError(f"{endpoint} HTTP {resp.status}: {raw[:200]}")
        return await resp.json(content_type=None)


async def _get_updates(
    session: aiohttp.ClientSession,
    *,
    base_url: str,
    token: str,
    sync_buf: str,
    timeout_ms: int,
) -> dict[str, Any]:
    return await _api_post(
        session,
        base_url=base_url,
        endpoint=_EP_GET_UPDATES,
        payload={"get_updates_buf": sync_buf, "longpolling_timeout_ms": timeout_ms},
        token=token,
        timeout_ms=timeout_ms,
    )


async def _send_message(
    session: aiohttp.ClientSession,
    *,
    base_url: str,
    token: str,
    to: str,
    text: str,
    context_token: str | None,
) -> dict[str, Any]:
    msg: dict[str, Any] = {
        "from_user_id": "",
        "to_user_id": to,
        "client_id": uuid.uuid4().hex,
        "message_type": _MSG_TYPE_BOT,
        "message_state": _MSG_STATE_FINISH,
        "item_list": [{"type": _ITEM_TEXT, "text_item": {"text": text}}],
    }
    if context_token:
        msg["context_token"] = context_token
    return await _api_post(
        session,
        base_url=base_url,
        endpoint=_EP_SEND_MESSAGE,
        payload={"msg": msg},
        token=token,
        timeout_ms=_API_TIMEOUT_MS,
    )


def _cdn_upload_url(cdn_base_url: str, upload_param: str, filekey: str) -> str:
    from urllib.parse import quote

    return (
        f"{cdn_base_url.rstrip('/')}/upload"
        f"?encrypted_query_param={quote(upload_param, safe='')}"
        f"&filekey={quote(filekey, safe='')}"
    )


async def _get_upload_url(
    session: aiohttp.ClientSession,
    *,
    base_url: str,
    token: str,
    to_user_id: str,
    media_type: int,
    filekey: str,
    rawsize: int,
    rawfilemd5: str,
    filesize: int,
    aeskey_hex: str,
) -> dict[str, Any]:
    """Request a CDN upload URL via the iLink getuploadurl endpoint."""
    return await _api_post(
        session,
        base_url=base_url,
        endpoint=_EP_GET_UPLOAD_URL,
        payload={
            "filekey": filekey,
            "media_type": media_type,
            "to_user_id": to_user_id,
            "rawsize": rawsize,
            "rawfilemd5": rawfilemd5,
            "filesize": filesize,
            "no_need_thumb": True,
            "aeskey": aeskey_hex,
        },
        token=token,
        timeout_ms=_API_TIMEOUT_MS,
    )


async def _upload_ciphertext(
    session: aiohttp.ClientSession,
    *,
    ciphertext: bytes,
    upload_url: str,
) -> str:
    """Upload encrypted media to the CDN; return the x-encrypted-param token.

    Both ``upload_full_url`` (direct CDN) and the constructed CDN URL use POST
    with the raw ciphertext as the body. ``asyncio.wait_for`` is used instead of
    aiohttp's ClientTimeout to avoid "Timeout context manager" errors when
    invoked from non-task contexts.
    """
    async def _do_upload() -> str:
        async with session.post(
            upload_url, data=ciphertext, headers={"Content-Type": "application/octet-stream"}
        ) as response:
            if response.status == 200:
                encrypted_param = response.headers.get("x-encrypted-param")
                if encrypted_param:
                    await response.read()
                    return encrypted_param
                raw = await response.text()
                raise RuntimeError(f"CDN upload missing x-encrypted-param header: {raw[:200]}")
            raw = await response.text()
            raise RuntimeError(f"CDN upload HTTP {response.status}: {raw[:200]}")

    return await asyncio.wait_for(_do_upload(), timeout=120)


# ── Deduplicator & ContextTokenStore ─────────────────────────────────────────

class _MessageDeduplicator:
    def __init__(self, ttl: float = _DEDUP_TTL):
        self._ttl = ttl
        self._seen: dict[str, float] = {}

    def is_duplicate(self, message_id: str) -> bool:
        now = time.time()
        self._seen = {k: v for k, v in self._seen.items() if now - v < self._ttl}
        if message_id in self._seen:
            return True
        self._seen[message_id] = now
        return False


class _ContextTokenStore:
    def __init__(self, data_dir: Path):
        self._dir = data_dir
        self._cache: dict[str, str] = {}

    def _path(self, account_id: str) -> Path:
        return self._dir / f"{account_id}.context-tokens.json"

    def restore(self, account_id: str) -> None:
        path = self._path(account_id)
        if not path.exists():
            return
        try:
            data = json.loads(path.read_text())
            for uid, tok in data.items():
                if isinstance(tok, str) and tok:
                    self._cache[f"{account_id}:{uid}"] = tok
        except Exception as exc:
            logger.warning("weixin: failed to restore context tokens: {}", exc)

    def get(self, account_id: str, user_id: str) -> str | None:
        return self._cache.get(f"{account_id}:{user_id}")

    def set(self, account_id: str, user_id: str, token: str) -> None:
        self._cache[f"{account_id}:{user_id}"] = token
        self._persist(account_id)

    def _persist(self, account_id: str) -> None:
        prefix = f"{account_id}:"
        payload = {k[len(prefix):]: v for k, v in self._cache.items() if k.startswith(prefix)}
        try:
            path = self._path(account_id)
            _atomic_write_json(path, payload)
        except Exception as exc:
            logger.warning("weixin: failed to persist context tokens: {}", exc)


def _extract_text(item_list: list[dict[str, Any]]) -> tuple[str, str | None, str | None]:
    """提取消息正文，并把被引用消息拆出来单独返回。

    返回 (text, reply_to_text, reply_to_sender)：
    引用上下文不再拼进 text，而是交给 pipeline 统一注入，与其他通道行为一致。
    """
    for item in item_list:
        if item.get("type") == _ITEM_TEXT:
            text = str((item.get("text_item") or {}).get("text") or "")
            ref = item.get("ref_msg") or {}
            ref_item = ref.get("message_item") or {}
            if ref_item:
                ref_text_item = ref_item if ref_item.get("type") == _ITEM_TEXT else None
                if ref_text_item:
                    inner = str((ref_text_item.get("text_item") or {}).get("text") or "")
                    title = ref.get("title") or ""
                    if inner or title:
                        return text, (inner or None), (title or None)
            return text, None, None
    for item in item_list:
        if item.get("type") == _ITEM_VOICE:
            return str((item.get("voice_item") or {}).get("text") or ""), None, None
    return "", None, None


def _guess_chat_type(message: dict[str, Any], account_id: str) -> tuple[str, str]:
    room_id = str(message.get("room_id") or message.get("chat_room_id") or "").strip()
    if room_id:
        return "group", room_id
    return "dm", str(message.get("from_user_id") or "")


def _media_reference(item: dict[str, Any], key: str) -> dict[str, Any]:
    return (item.get(key) or {}).get("media") or {}


def _load_sync_buf(data_dir: Path, account_id: str) -> str:
    path = data_dir / f"{account_id}.sync.json"
    if not path.exists():
        return ""
    try:
        data = json.loads(path.read_text())
        return str(data.get("sync_buf") or "")
    except Exception as e:
        logger.debug("Failed to load sync buffer for {}: {}", account_id, e)
        return ""


def _atomic_write_json(path: Path, payload: dict[str, Any]) -> None:
    """Replace state only after a complete write; ENOSPC must keep old tokens."""
    descriptor, temporary = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    try:
        with os.fdopen(descriptor, "w", encoding="utf-8") as handle:
            json.dump(payload, handle, ensure_ascii=False)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, path)
    finally:
        Path(temporary).unlink(missing_ok=True)


def _save_sync_buf(data_dir: Path, account_id: str, sync_buf: str) -> None:
    path = data_dir / f"{account_id}.sync.json"
    try:
        _atomic_write_json(path, {"sync_buf": sync_buf})
    except Exception as exc:
        logger.warning("weixin: failed to save sync_buf: {}", exc)


# ── Channel implementation ───────────────────────────────────────────────────

class WeixinChannel(BaseChannel):
    name = "weixin"
    # Consumes structured IMAGE/FILE/AUDIO/VIDEO blocks via the WeChat CDN upload
    # path, so send_file delivers a real attachment here.
    supports_files = True

    def __init__(self, config: WeixinChannelConfig, bus: MessageBus):
        super().__init__(config, bus)
        self._account_id = config.account_id
        self._token = config.token
        self._base_url = config.base_url.rstrip("/")
        self._cdn_base_url = config.cdn_base_url.rstrip("/")
        self._dm_policy = config.dm_policy
        data_dir = config.data_dir or os.path.expanduser("~/.echo-agent/weixin")
        self._data_dir = Path(data_dir)
        self._data_dir.mkdir(parents=True, exist_ok=True)
        self._dedup = _MessageDeduplicator()
        self._token_store = _ContextTokenStore(self._data_dir)
        self._poll_session: aiohttp.ClientSession | None = None
        self._send_session: aiohttp.ClientSession | None = None
        self._poll_task: asyncio.Task | None = None
        # Strong refs for per-message tasks (see asyncio.create_task docs).
        self._msg_tasks: set[asyncio.Task] = set()

    def _spawn_msg_task(self, coro: Any) -> asyncio.Task:
        task = asyncio.create_task(coro)
        self._msg_tasks.add(task)
        task.add_done_callback(self._on_msg_task_done)
        return task

    def _on_msg_task_done(self, task: asyncio.Task) -> None:
        self._msg_tasks.discard(task)
        if not task.cancelled() and task.exception():
            logger.warning("weixin message task failed: {}", task.exception())

    async def start(self) -> None:
        if not self._token or not self._account_id:
            logger.error("weixin: account_id and token are required")
            return
        self._poll_session = aiohttp.ClientSession(trust_env=True)
        self._send_session = aiohttp.ClientSession(trust_env=True)
        self._token_store.restore(self._account_id)
        self._running = True
        self._poll_task = asyncio.create_task(self._poll_loop(), name="weixin-poll")
        self.bus.subscribe_outbound(self.name, self.send)
        logger.info("weixin channel started, account={}", self._account_id[:8] if self._account_id else "?")

    async def stop(self) -> None:
        self._running = False
        if self._poll_task and not self._poll_task.done():
            self._poll_task.cancel()
            try:
                await asyncio.wait_for(asyncio.shield(self._poll_task), timeout=5)
            except (asyncio.CancelledError, asyncio.TimeoutError):
                # Cancellation is requested above; a timeout only bounds shutdown
                # of an uncooperative long poll before sessions are closed.
                pass
        self._poll_task = None
        if self._msg_tasks:
            tasks = list(self._msg_tasks)
            for task in tasks:
                task.cancel()
            await asyncio.gather(*tasks, return_exceptions=True)
            self._msg_tasks.clear()
        if self._poll_session and not self._poll_session.closed:
            await self._poll_session.close()
        self._poll_session = None
        if self._send_session and not self._send_session.closed:
            await self._send_session.close()
        self._send_session = None
        logger.info("weixin channel stopped")

    # ── Send ─────────────────────────────────────────────────────────────────

    async def send(self, event: OutboundEvent) -> SendResult | None:
        if not self.should_deliver(event):
            return SendResult(success=True, skipped=True)
        if not self._send_session or not self._token:
            return SendResult(success=False, error="no session or no token")
        chat_id = event.chat_id

        # Route each content block: text via sendmessage, image/file via CDN upload.
        # Fall back to event.text when no structured blocks are present.
        blocks = list(event.content) or []
        has_media = any(
            b.url and b.type in (ContentType.IMAGE, ContentType.FILE, ContentType.AUDIO, ContentType.VIDEO)
            for b in blocks
        )
        if not has_media:
            return await self._send_text(chat_id, event.text or "", event=event)

        parts = []
        for block in blocks:
            if block.type == ContentType.TEXT:
                parts.extend(json.dumps(["text", chunk, ""], ensure_ascii=False) for chunk in self._split_text(block.text or ""))
            elif block.url:
                parts.append(json.dumps([block.type.value, "", block.url], ensure_ascii=False))
        async def send_part(index, part):
            block_type, text, source = json.loads(part)
            block_type = ContentType(block_type)
            if block_type == ContentType.TEXT:
                return await self._send_text_chunk(chat_id, text)
            if block_type == ContentType.AUDIO and source:
                return await self.send_voice(chat_id, source)
            if source and block_type in (
                ContentType.IMAGE, ContentType.FILE, ContentType.VIDEO,
            ):
                return await self.send_file(chat_id, source, as_image=block_type == ContentType.IMAGE)
            return SendResult(success=False, error="unsupported Weixin content block")
        return await self.send_parts(event, parts, send_part, namespace="blocks", force_checkpoint=True)

    async def _send_text(self, chat_id: str, text: str, *, event: OutboundEvent | None = None) -> SendResult:
        if not text:
            return SendResult(success=True, skipped=True)
        if not self._send_session or not self._token:
            return SendResult(success=False, error="no session or no token")
        context_token = self._token_store.get(self._account_id, chat_id)
        chunks = self._split_text(text)
        async def send_part(index, chunk):
            return await self._send_text_chunk(chat_id, chunk, context_token=context_token)

        async def before_send(index, chunk):
            if index > 0:
                await asyncio.sleep(0.3)
        event = event or OutboundEvent.text_reply(self.name, chat_id, text)
        return await self.send_parts(event, chunks, send_part, before_send=before_send)

    async def _send_text_chunk(self, chat_id: str, chunk: str, *, context_token: str | None = None) -> SendResult:
        if context_token is None:
            context_token = self._token_store.get(self._account_id, chat_id)
        try:
            resp = await _send_message(
                self._send_session,
                base_url=self._base_url,
                token=self._token,
                to=chat_id,
                text=chunk,
                context_token=context_token,
            )
            failure = _require_ok(resp, what="sendmessage")
            if not isinstance(resp, dict) or not (any(key in resp for key in ("ret", "errcode")) or _server_message_id(resp)):
                return SendResult(success=False, error=failure or "Weixin acknowledgement missing", delivery_unknown=True)
            if failure:
                logger.warning("weixin send error: {}", failure)
                return SendResult(success=False, error=failure)
        except Exception as exc:
            error = self.safe_delivery_error(exc)
            logger.error("weixin send failed to {}: {}", chat_id[:8] if chat_id else "?", error)
            return SendResult(success=False, error=error, delivery_unknown=True)
        return SendResult(success=True, message_id=_server_message_id(resp))

    @staticmethod
    def _split_text(text: str) -> list[str]:
        return split_message(text, _MAX_MESSAGE_LENGTH, preserve_whitespace=True)

    # ── Send file / image ──────────────────────────────────────────────────────

    async def send_file(
        self,
        chat_id: str,
        source: str,
        *,
        caption: str = "",
        as_image: bool | None = None,
    ) -> SendResult:
        """Send a local-path or http(s) file/image to *chat_id*.

        ``as_image`` forces image vs. file-attachment rendering; when ``None`` it
        is inferred from the MIME type (``image/*`` → image). Remote URLs are
        downloaded to a temp file first and cleaned up afterward.
        """
        if not self._send_session or not self._token:
            return SendResult(success=False, error="no session or no token")
        path, cleanup = await self._materialize_source(source)
        if not path:
            return SendResult(success=False, error=f"could not resolve media source: {source[:80]}")
        try:
            if as_image is None:
                mime = mimetypes.guess_type(path)[0] or ""
                as_image = mime.startswith("image/")
            if caption:
                await self._send_text(chat_id, caption)
            return await self._send_file(chat_id, path, as_image=as_image)
        except Exception as exc:
            logger.error("weixin send_file failed to {}: {}", chat_id[:8] if chat_id else "?", exc)
            return SendResult(success=False, error=str(exc))
        finally:
            if cleanup and path and os.path.exists(path):
                try:
                    os.unlink(path)
                except OSError:
                    # This is a per-send temporary download; cleanup failure must
                    # not replace the already determined delivery result.
                    pass

    async def _materialize_source(self, source: str) -> tuple[str, bool]:
        """Return (local_path, needs_cleanup). Downloads http(s) to a temp file."""
        source = (source or "").strip()
        if not source:
            return "", False
        if source.startswith(("http://", "https://")):
            assert self._send_session is not None

            async def _do_fetch() -> bytes:
                async with self._send_session.get(source) as resp:
                    resp.raise_for_status()
                    return await resp.read()

            data = await asyncio.wait_for(_do_fetch(), timeout=30)
            suffix = Path(source.split("?", 1)[0]).suffix or ".bin"
            with tempfile.NamedTemporaryFile(delete=False, suffix=suffix) as handle:
                handle.write(data)
                return handle.name, True
        local = source.replace("file://", "")
        if not os.path.isabs(local):
            local = os.path.abspath(local)
        if not os.path.exists(local):
            return "", False
        return local, False

    async def _send_file(
        self, chat_id: str, path: str, *,
        as_image: bool = False, as_voice: bool = False, voice_ms: int = 0,
    ) -> SendResult:
        """Encrypt, upload to the CDN, and deliver one media item via sendmessage."""
        assert self._send_session is not None and self._token is not None
        plaintext = Path(path).read_bytes()
        rawsize = len(plaintext)
        rawfilemd5 = hashlib.md5(plaintext).hexdigest()  # noqa: S324 - protocol-mandated
        filekey = secrets.token_hex(16)
        aes_key = secrets.token_bytes(16)
        if as_voice:
            media_type = _MEDIA_VOICE
        elif as_image:
            media_type = _MEDIA_IMAGE
        else:
            media_type = _MEDIA_FILE

        upload_response = await _get_upload_url(
            self._send_session,
            base_url=self._base_url,
            token=self._token,
            to_user_id=chat_id,
            media_type=media_type,
            filekey=filekey,
            rawsize=rawsize,
            rawfilemd5=rawfilemd5,
            filesize=_aes_padded_size(rawsize),
            aeskey_hex=aes_key.hex(),
        )
        failure = _require_ok(upload_response, what="getuploadurl")
        if failure:
            return SendResult(success=False, error=f"getuploadurl {failure}")

        upload_full_url = str(upload_response.get("upload_full_url") or "")
        upload_param = str(upload_response.get("upload_param") or "")
        if upload_full_url:
            upload_url = upload_full_url
        elif upload_param:
            upload_url = _cdn_upload_url(self._cdn_base_url, upload_param, filekey)
        else:
            return SendResult(success=False, error=f"getuploadurl returned no upload URL: {upload_response}")

        ciphertext = _aes128_ecb_encrypt(plaintext, aes_key)
        encrypted_query_param = await _upload_ciphertext(
            self._send_session, ciphertext=ciphertext, upload_url=upload_url,
        )

        # The iLink API expects aes_key as base64(hex_string), not base64(raw_bytes);
        # the latter renders as grey boxes on the receiver side.
        aes_key_for_api = base64.b64encode(aes_key.hex().encode("ascii")).decode("ascii")
        media = {
            "encrypt_query_param": encrypted_query_param,
            "aes_key": aes_key_for_api,
            "encrypt_type": 1,
        }
        if as_voice:
            item = {
                "type": _ITEM_VOICE,
                "voice_item": {
                    "media": media,
                    "encode_type": 6,
                    "sample_rate": 24000,
                    "bits_per_sample": 16,
                    "playtime": voice_ms,
                },
            }
        elif as_image:
            item = {"type": _ITEM_IMAGE, "image_item": {"media": media, "mid_size": len(ciphertext)}}
        else:
            item = {
                "type": _ITEM_FILE,
                "file_item": {"media": media, "file_name": Path(path).name, "len": str(rawsize)},
            }

        context_token = self._token_store.get(self._account_id, chat_id)
        msg: dict[str, Any] = {
            "from_user_id": "",
            "to_user_id": chat_id,
            "client_id": uuid.uuid4().hex,
            "message_type": _MSG_TYPE_BOT,
            "message_state": _MSG_STATE_FINISH,
            "item_list": [item],
        }
        if context_token:
            msg["context_token"] = context_token
        try:
            resp = await _api_post(
                self._send_session,
                base_url=self._base_url,
                endpoint=_EP_SEND_MESSAGE,
                payload={"msg": msg},
                token=self._token,
                timeout_ms=_API_TIMEOUT_MS,
            )
        except Exception as exc:
            return SendResult(success=False, error=self.safe_delivery_error(exc), delivery_unknown=True)
        failure = _require_ok(resp, what="sendmessage")
        if not isinstance(resp, dict) or not (any(key in resp for key in ("ret", "errcode")) or _server_message_id(resp)):
            return SendResult(success=False, error=failure or "Weixin acknowledgement missing", delivery_unknown=True)
        if failure:
            logger.warning("weixin media send error: {}", failure)
            return SendResult(success=False, error=f"sendmessage {failure}")
        # Prefer the server receipt; code-only responses keep the local id
        # solely for event correlation.
        return SendResult(success=True, message_id=_server_message_id(resp) or str(msg["client_id"]))

    async def send_voice(
        self, chat_id: str, audio_source: str, metadata: dict[str, Any] | None = None,
    ) -> SendResult:
        """Send a native Weixin voice bubble; fall back to file attachment on any failure."""
        if not self._send_session or not self._token:
            return SendResult(success=False, error="no session or no token")
        path, cleanup = await self._materialize_source(audio_source)
        if not path:
            return SendResult(success=False, error=f"could not resolve audio source: {audio_source[:80]}")
        silk_path: str | None = None
        try:
            silk_path, duration_ms = await encode_to_silk(path)
            if duration_ms > 60_000:
                logger.warning("weixin voice >60s ({}ms), sending as file attachment", duration_ms)
                return await self._send_file(chat_id, path, as_image=False)
            res = await self._send_file(chat_id, silk_path, as_voice=True, voice_ms=duration_ms)
            if res.delivery_unknown:
                return res  # A voice may already exist; never send a fallback copy.
            if not res.success:
                logger.warning("weixin native voice failed ({}), falling back to file", res.error)
                return await self._send_file(chat_id, path, as_image=False)
            return res
        except Exception as exc:
            logger.warning("weixin voice encode/send failed, falling back to file: {}", exc)
            try:
                return await self._send_file(chat_id, path, as_image=False)
            except Exception as exc2:
                return SendResult(success=False, error=str(exc2))
        finally:
            if silk_path and os.path.exists(silk_path):
                try:
                    os.unlink(silk_path)
                except OSError:
                    # Encoded voice output is disposable cleanup after delivery;
                    # preserve the send/fallback result if deletion fails.
                    pass
            if cleanup and path and os.path.exists(path):
                try:
                    os.unlink(path)
                except OSError:
                    # The materialized source is temporary and cleanup is
                    # best-effort after the delivery outcome is final.
                    pass

    # ── Poll loop ────────────────────────────────────────────────────────────

    async def _poll_loop(self) -> None:
        assert self._poll_session is not None
        sync_buf = _load_sync_buf(self._data_dir, self._account_id)
        timeout_ms = _LONG_POLL_TIMEOUT_MS
        consecutive_failures = 0
        last_message_time = time.monotonic()

        while self._running:
            try:
                response = await _get_updates(
                    self._poll_session,
                    base_url=self._base_url,
                    token=self._token,
                    sync_buf=sync_buf,
                    timeout_ms=timeout_ms,
                )
                suggested = response.get("longpolling_timeout_ms")
                if isinstance(suggested, int) and suggested > 0:
                    timeout_ms = suggested

                # 与发送路径共用 _api_error:两条路径对"什么算失败"必须同源,
                # 否则又会出现同一个响应轮询侧致命、发送侧当成功的分歧。
                failure = _api_error(response)
                if failure:
                    if _is_session_expired(response):
                        logger.error("weixin: session expired, pausing 10 minutes")
                        await asyncio.sleep(600)
                        consecutive_failures = 0
                        continue
                    consecutive_failures += 1
                    delay = _BACKOFF_DELAY if consecutive_failures >= _MAX_CONSECUTIVE_FAILURES else _RETRY_DELAY
                    logger.warning("weixin: poll error {} ({}/{})", failure, consecutive_failures, _MAX_CONSECUTIVE_FAILURES)
                    await asyncio.sleep(delay)
                    if consecutive_failures >= _MAX_CONSECUTIVE_FAILURES:
                        consecutive_failures = 0
                    continue

                consecutive_failures = 0
                new_sync_buf = str(response.get("get_updates_buf") or "")

                msgs = response.get("msgs") or []
                if msgs:
                    last_message_time = time.monotonic()
                batch_tasks = []
                for message in msgs:
                    task = self._spawn_msg_task(self._process_message_safe(message))
                    batch_tasks.append(task)

                # Wait for message processing to complete before advancing cursor.
                # This closes the crash-loss window where sync_buf advances past
                # messages that were never processed.
                if batch_tasks:
                    outcomes = await asyncio.gather(*batch_tasks, return_exceptions=True)
                    if any(outcome is False or isinstance(outcome, BaseException) for outcome in outcomes):
                        await asyncio.sleep(_RETRY_DELAY)
                        continue

                if new_sync_buf:
                    sync_buf = new_sync_buf
                    _save_sync_buf(self._data_dir, self._account_id, sync_buf)

                # 活性检测：长时间无消息时重建连接
                if time.monotonic() - last_message_time > _LIVENESS_TIMEOUT:
                    logger.warning("weixin: no messages for {}s, rebuilding session", _LIVENESS_TIMEOUT)
                    await self._poll_session.close()
                    # trust_env=True 必须跟 start() 里一致:靠 HTTPS_PROXY 出网的
                    # 部署,重建后的会话不带这个参数就再也不走代理,轮询从此静默
                    # 收不到消息 —— 而这条重建日志恰好长得像"一切正常"。
                    self._poll_session = aiohttp.ClientSession(trust_env=True)
                    last_message_time = time.monotonic()
            except asyncio.CancelledError:
                break
            except Exception as exc:
                consecutive_failures += 1
                delay = _BACKOFF_DELAY if consecutive_failures >= _MAX_CONSECUTIVE_FAILURES else _RETRY_DELAY
                logger.error("weixin: poll exception ({}/{}): {}", consecutive_failures, _MAX_CONSECUTIVE_FAILURES, exc)
                await asyncio.sleep(delay)
                if consecutive_failures >= _MAX_CONSECUTIVE_FAILURES:
                    consecutive_failures = 0

    # ── Message processing ───────────────────────────────────────────────────

    async def _process_message_safe(self, message: dict[str, Any]) -> bool:
        try:
            return await self._process_message(message)
        except Exception as exc:
            logger.error("weixin: unhandled inbound error: {}", exc, exc_info=True)
            message_id = str(message.get("message_id") or "").strip()
            self._dedup._seen.pop(message_id, None)
            return False

    async def _process_message(self, message: dict[str, Any]) -> bool:
        sender_id = str(message.get("from_user_id") or "").strip()
        if not sender_id or sender_id == self._account_id:
            return True

        message_id = str(message.get("message_id") or "").strip()
        if message_id and self._dedup.is_duplicate(message_id):
            return True

        chat_type, effective_chat_id = _guess_chat_type(message, self._account_id)
        if chat_type == "group":
            return True
        if self._dm_policy == "disabled":
            return True
        if not self.is_allowed(sender_id):
            return True

        context_token = str(message.get("context_token") or "").strip()
        if context_token:
            self._token_store.set(self._account_id, sender_id, context_token)

        item_list = message.get("item_list") or []
        text, reply_to_text, reply_to_sender = _extract_text(item_list)

        media: list[dict[str, str]] = []
        placeholders: list[str] = []
        for item in item_list:
            m = self._extract_media_info(item)
            if not m:
                continue
            if m.get("url"):
                media.append(m)
            elif m.get("label"):
                placeholders.append(m["label"])

        # 媒体存在但拿不到下载地址时（CDN 链接缺失/过期），不要静默丢弃，
        # 而是用占位文本告知用户收到了文件。
        if placeholders:
            notice = "\n".join(placeholders)
            text = f"{text}\n{notice}".strip() if text else notice

        if not text and not media:
            return True

        inbound = await self._handle_message(
            sender_id=sender_id,
            chat_id=effective_chat_id,
            text=text,
            media=media if media else None,
            reply_to_text=reply_to_text,
            reply_to_sender=reply_to_sender,
            metadata={"message_id": message_id, "chat_type": chat_type},
            event_id=self._native_event_id(effective_chat_id, message_id),
        )
        if inbound is None and message_id:
            self._dedup._seen.pop(message_id, None)
        return inbound is not None

    def _resolve_media_url(self, url: str) -> str:
        """Normalize a media URL, joining relative paths against the CDN base."""
        url = (url or "").strip()
        if not url:
            return ""
        if url.startswith(("http://", "https://")):
            return url
        if url.startswith("//"):
            return f"https:{url}"
        if self._cdn_base_url:
            return f"{self._cdn_base_url}/{url.lstrip('/')}"
        return url

    def _extract_media_info(self, item: dict[str, Any]) -> dict[str, str] | None:
        item_type = item.get("type")
        if item_type == _ITEM_IMAGE:
            ref = _media_reference(item, "image_item")
            url = self._resolve_media_url(ref.get("full_url") or "")
            if url:
                result: dict[str, str] = {"type": "image", "url": url}
                aes_key = ref.get("aes_key") or ref.get("decode_key") or ""
                if aes_key:
                    result["aes_key"] = aes_key
                return result
            return {"type": "image", "label": "[收到图片]"}
        if item_type == _ITEM_FILE:
            name = (item.get("file_item") or {}).get("file_name") or "file"
            ref = _media_reference(item, "file_item")
            url = self._resolve_media_url(ref.get("full_url") or "")
            if url:
                result = {"type": "file", "url": url, "name": name}
                aes_key = ref.get("aes_key") or ref.get("decode_key") or ""
                if aes_key:
                    result["aes_key"] = aes_key
                return result
            return {"type": "file", "label": f"[收到文件: {name}]"}
        if item_type == _ITEM_VIDEO:
            ref = _media_reference(item, "video_item")
            url = self._resolve_media_url(ref.get("full_url") or "")
            if url:
                result = {"type": "video", "url": url}
                aes_key = ref.get("aes_key") or ref.get("decode_key") or ""
                if aes_key:
                    result["aes_key"] = aes_key
                return result
            return {"type": "video", "label": "[收到视频]"}
        # 语音消息的转写文本已由 _extract_text 处理，无需作为附件重复采集。
        return None

    # ── QR login (static, for CLI use) ───────────────────────────────────────

    @staticmethod
    async def qr_login(
        base_url: str = "https://ilinkai.weixin.qq.com",
        timeout_seconds: int = 480,
    ) -> dict[str, str] | None:
        base_url = base_url.rstrip("/")
        async with aiohttp.ClientSession(trust_env=True) as session:
            url = f"{base_url}/{_EP_GET_BOT_QR}"
            async with session.get(url, params={"bot_type": "3"}, headers=_login_headers()) as resp:
                if resp.status != 200:
                    logger.error("weixin: get_bot_qrcode HTTP {}: {}", resp.status, await resp.text())
                    return None
                data = await resp.json(content_type=None)

            errcode = data.get("errcode") or data.get("err_code")
            if errcode:
                logger.error("weixin: get_bot_qrcode returned error {}: {}", errcode, data.get("errmsg") or data.get("err_msg") or data)

            qrcode = data.get("qrcode")
            qr_url = data.get("qrcode_img_content")
            if not qrcode:
                logger.error("weixin: failed to get QR code, response: {}", data)
                return None

            print(f"\nScan this QR code with WeChat:\n{qr_url}\n")

            deadline = time.time() + timeout_seconds
            refresh_count = 0
            while time.time() < deadline:
                await asyncio.sleep(2)
                check_url = f"{base_url}/{_EP_GET_QR_STATUS}"
                async with session.get(check_url, params={"qrcode": qrcode}, headers=_login_headers()) as resp:
                    status_data = await resp.json(content_type=None)

                status = status_data.get("status", "")
                if status == "confirmed":
                    return {
                        "account_id": str(status_data.get("account_id") or status_data.get("ilink_bot_id") or ""),
                        "token": str(status_data.get("token") or status_data.get("bot_token") or ""),
                        "base_url": str(status_data.get("base_url") or status_data.get("baseurl") or base_url),
                        "user_id": str(status_data.get("user_id") or status_data.get("ilink_user_id") or ""),
                    }
                elif status == "expired":
                    refresh_count += 1
                    if refresh_count >= 3:
                        logger.error("weixin: QR code expired too many times")
                        return None
                    async with session.get(url, params={"bot_type": "3"}, headers=_login_headers()) as resp:
                        data = await resp.json(content_type=None)
                    qrcode = data.get("qrcode")
                    qr_url = data.get("qrcode_img_content")
                    if qrcode:
                        print(f"\nQR expired, scan new one:\n{qr_url}\n")
                elif status == "scaned":
                    pass

            logger.error("weixin: QR login timed out")
            return None
