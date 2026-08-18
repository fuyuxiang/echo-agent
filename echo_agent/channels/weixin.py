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
_EP_GET_CONFIG = "ilink/bot/getconfig"
_EP_SEND_TYPING = "ilink/bot/sendtyping"

_LONG_POLL_TIMEOUT_MS = 35_000
_API_TIMEOUT_MS = 15_000
_MAX_CONSECUTIVE_FAILURES = 3
_RETRY_DELAY = 2
_BACKOFF_DELAY = 30
_SESSION_EXPIRED_ERRCODE = -14
_DEDUP_TTL = 300
_MAX_MESSAGE_LENGTH = 4000
_LIVENESS_TIMEOUT = 30 * 60  # 30 分钟无消息视为静默失效

# 输入状态下发（ilink/bot/sendtyping）
_TYPING_START = 1               # status=1：显示“对方正在输入”
_TYPING_STOP = 2               # status=2：取消输入状态
_TYPING_REFRESH_INTERVAL = 3    # 秒，处理期间周期性补发以防输入状态过期
                                # （微信输入气泡约 5 秒过期，取 3 秒留余量，
                                # 避免慢网络下单次 sendtyping 延迟露出气泡空窗）
# 孤儿保护兜底：正常停止由 ChannelManager 在最终回复落地时调 stop_typing 完成，
# 心跳每个 beat 又会 send_typing 复活刷新循环；这个上限只在“stop_typing 因故
# 永不触发”（如最终回复事件丢失）时兜底，避免“正在输入”无限期刷下去。设得
# 远高于心跳 interval，循环到期后由 _on_typing_done 清槽、下个 beat 自动复活。
_TYPING_MAX_DURATION = 600      # 秒
_TYPING_TICKET_TTL = 500        # 秒，typing_ticket 缓存有效期。iLink 服务端实际
                                # 寿命约 600 秒，过期后 sendtyping 会被静默拒绝
                                # （连 status=2 停止也发不出去，导致气泡卡死），
                                # 故取 500 秒在过期前主动经 getconfig 重拉。

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


async def _get_config(
    session: aiohttp.ClientSession,
    *,
    base_url: str,
    token: str,
    user_id: str,
    context_token: str | None = None,
) -> dict[str, Any]:
    """Fetch bot runtime config, including the typing_ticket used by sendtyping.

    iLink binds the typing_ticket to the peer conversation, so getconfig must
    carry the peer's ``ilink_user_id`` (and the latest ``context_token`` when
    known); an empty body yields a response without a usable ticket.
    """
    payload: dict[str, Any] = {"ilink_user_id": user_id}
    if context_token:
        payload["context_token"] = context_token
    return await _api_post(
        session,
        base_url=base_url,
        endpoint=_EP_GET_CONFIG,
        payload=payload,
        token=token,
        timeout_ms=_API_TIMEOUT_MS,
    )


async def _send_typing(
    session: aiohttp.ClientSession,
    *,
    base_url: str,
    token: str,
    to: str,
    status: int,
    typing_ticket: str,
) -> dict[str, Any]:
    """Send a typing indicator (status=1 start, status=2 cancel) to *to*."""
    return await _api_post(
        session,
        base_url=base_url,
        endpoint=_EP_SEND_TYPING,
        payload={"ilink_user_id": to, "status": status, "typing_ticket": typing_ticket},
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
            path.write_text(json.dumps(payload, ensure_ascii=False))
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


def _save_sync_buf(data_dir: Path, account_id: str, sync_buf: str) -> None:
    path = data_dir / f"{account_id}.sync.json"
    try:
        path.write_text(json.dumps({"sync_buf": sync_buf}))
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
        # Typing indicator state.
        self._typing_enabled = config.typing_indicator
        # Per-chat typing_ticket cache: chat_id -> (ticket, fetched_at_monotonic).
        # iLink binds the ticket to the peer conversation, so a global single
        # ticket would cross wires between concurrent chats.
        self._typing_tickets: dict[str, tuple[str, float]] = {}
        # One refreshing typing-loop task per chat being processed.
        self._typing_tasks: dict[str, asyncio.Task] = {}

    def _spawn_msg_task(self, coro: Any) -> None:
        task = asyncio.create_task(coro)
        self._msg_tasks.add(task)
        task.add_done_callback(self._on_msg_task_done)

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
                pass
        self._poll_task = None
        if self._msg_tasks:
            tasks = list(self._msg_tasks)
            for task in tasks:
                task.cancel()
            await asyncio.gather(*tasks, return_exceptions=True)
            self._msg_tasks.clear()
        # Cancel typing loops before sessions close so their status=2 send can run.
        if self._typing_tasks:
            typing_tasks = list(self._typing_tasks.values())
            for task in typing_tasks:
                task.cancel()
            await asyncio.gather(*typing_tasks, return_exceptions=True)
            self._typing_tasks.clear()
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
            return await self._send_text(chat_id, event.text or "")

        all_ok = True
        last_error = ""
        for block in blocks:
            if block.type == ContentType.TEXT:
                if block.text:
                    res = await self._send_text(chat_id, block.text)
                    if not res.success:
                        all_ok, last_error = False, res.error
            elif block.type == ContentType.AUDIO and block.url:
                res = await self.send_voice(chat_id, block.url)
                if not res.success:
                    all_ok, last_error = False, res.error
            elif block.url and block.type in (
                ContentType.IMAGE, ContentType.FILE, ContentType.VIDEO,
            ):
                as_image = block.type == ContentType.IMAGE
                res = await self.send_file(chat_id, block.url, as_image=as_image)
                if not res.success:
                    all_ok, last_error = False, res.error
        return SendResult(success=all_ok, error="" if all_ok else last_error)

    async def _send_text(self, chat_id: str, text: str) -> SendResult:
        if not text:
            return SendResult(success=True, skipped=True)
        if not self._send_session or not self._token:
            return SendResult(success=False, error="no session or no token")
        context_token = self._token_store.get(self._account_id, chat_id)
        chunks = self._split_text(text)
        for chunk in chunks:
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
                if failure:
                    logger.warning("weixin send error: {}", failure)
                    return SendResult(success=False, error=failure)
            except Exception as exc:
                logger.error("weixin send failed to {}: {}", chat_id[:8] if chat_id else "?", exc)
                return SendResult(success=False, error=str(exc))
            if len(chunks) > 1:
                await asyncio.sleep(0.3)
        return SendResult(success=True)

    @staticmethod
    def _split_text(text: str) -> list[str]:
        return split_message(text, _MAX_MESSAGE_LENGTH)

    # ── Typing indicator ───────────────────────────────────────────────────────

    async def _ensure_typing_ticket(self, chat_id: str) -> str | None:
        """Return a cached typing_ticket for *chat_id*, fetching via getconfig if needed."""
        if not self._send_session or not self._token:
            return None
        now = time.monotonic()
        cached = self._typing_tickets.get(chat_id)
        if cached and now - cached[1] < _TYPING_TICKET_TTL:
            return cached[0]
        context_token = self._token_store.get(self._account_id, chat_id)
        try:
            resp = await _get_config(
                self._send_session,
                base_url=self._base_url,
                token=self._token,
                user_id=chat_id,
                context_token=context_token,
            )
        except Exception as exc:
            logger.debug("weixin: getconfig for typing_ticket failed: {}", exc)
            return None
        ticket = str(resp.get("typing_ticket") or (resp.get("config") or {}).get("typing_ticket") or "")
        if not ticket:
            logger.debug("weixin: getconfig returned no typing_ticket: {}", resp)
            return None
        self._typing_tickets[chat_id] = (ticket, now)
        return ticket

    async def _do_send_typing(self, chat_id: str, status: int) -> None:
        ticket = await self._ensure_typing_ticket(chat_id)
        if not ticket or not self._send_session:
            return
        try:
            await _send_typing(
                self._send_session,
                base_url=self._base_url,
                token=self._token,
                to=chat_id,
                status=status,
                typing_ticket=ticket,
            )
        except Exception as exc:
            logger.debug("weixin: sendtyping status={} to {} failed: {}", status, chat_id[:8] if chat_id else "?", exc)

    async def _typing_loop(self, chat_id: str) -> None:
        """Keep 'typing' shown for *chat_id* until cancelled or the max window elapses.

        The indicator is re-sent periodically because the input state can expire
        before a slow agent reply is ready; on exit (cancel or timeout) it sends
        status=2 so the indicator never gets stuck when no reply is produced.
        """
        deadline = time.monotonic() + _TYPING_MAX_DURATION
        try:
            while time.monotonic() < deadline:
                await self._do_send_typing(chat_id, _TYPING_START)
                await asyncio.sleep(_TYPING_REFRESH_INTERVAL)
        except asyncio.CancelledError:
            pass
        finally:
            # Shield the stop send: this runs during cancellation, and a *second*
            # cancel (e.g. stop_typing followed by channel stop()) would otherwise
            # abort the await and leave "typing" stuck on the peer's screen.
            await asyncio.shield(
                asyncio.ensure_future(self._do_send_typing(chat_id, _TYPING_STOP))
            )

    async def send_typing(self, chat_id: str, metadata: dict[str, Any] | None = None) -> None:
        """Start (or keep) the typing indicator for *chat_id*.

        Implements the BaseChannel primitive driven by ChannelManager: it is
        called on inbound and on every heartbeat beat. Idempotent per chat — a
        beat arriving while a refresh loop is already running is a no-op, and a
        beat after the loop self-terminated (orphan-cap) revives it.
        """
        if not self._typing_enabled or not chat_id:
            return
        existing = self._typing_tasks.get(chat_id)
        if existing and not existing.done():
            return  # already showing typing for this chat
        task = asyncio.create_task(self._typing_loop(chat_id))
        self._typing_tasks[chat_id] = task
        task.add_done_callback(self._on_typing_done)

    async def stop_typing(self, chat_id: str) -> None:
        """Stop the typing indicator for *chat_id* (sends status=2 via the loop's finally)."""
        if not self._typing_enabled or not chat_id:
            return
        task = self._typing_tasks.pop(chat_id, None)
        if task and not task.done():
            task.cancel()  # _typing_loop's finally sends status=2

    def _on_typing_done(self, task: asyncio.Task) -> None:
        for cid, t in list(self._typing_tasks.items()):
            if t is task:
                self._typing_tasks.pop(cid, None)
                break
        if not task.cancelled() and task.exception():
            logger.debug("weixin typing task failed: {}", task.exception())

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
        resp = await _api_post(
            self._send_session,
            base_url=self._base_url,
            endpoint=_EP_SEND_MESSAGE,
            payload={"msg": msg},
            token=self._token,
            timeout_ms=_API_TIMEOUT_MS,
        )
        failure = _require_ok(resp, what="sendmessage")
        if failure:
            logger.warning("weixin media send error: {}", failure)
            return SendResult(success=False, error=f"sendmessage {failure}")
        # client_id 是本地生成的 uuid,不是服务端回执。留作事件关联用,但不要
        # 拿它当"平台确认送达"的凭据:服务端 message_id 目前不在响应里解析。
        return SendResult(success=True, message_id=str(msg["client_id"]))

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
                    pass
            if cleanup and path and os.path.exists(path):
                try:
                    os.unlink(path)
                except OSError:
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
                if new_sync_buf:
                    sync_buf = new_sync_buf
                    _save_sync_buf(self._data_dir, self._account_id, sync_buf)

                msgs = response.get("msgs") or []
                if msgs:
                    last_message_time = time.monotonic()
                for message in msgs:
                    self._spawn_msg_task(self._process_message_safe(message))

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

    async def _process_message_safe(self, message: dict[str, Any]) -> None:
        try:
            await self._process_message(message)
        except Exception as exc:
            logger.error("weixin: unhandled inbound error: {}", exc, exc_info=True)

    async def _process_message(self, message: dict[str, Any]) -> None:
        sender_id = str(message.get("from_user_id") or "").strip()
        if not sender_id or sender_id == self._account_id:
            return

        message_id = str(message.get("message_id") or "").strip()
        if message_id and self._dedup.is_duplicate(message_id):
            return

        chat_type, effective_chat_id = _guess_chat_type(message, self._account_id)
        if chat_type == "group":
            return
        if self._dm_policy == "disabled":
            return
        if self._dm_policy == "allowlist" and not self.is_allowed(sender_id):
            return

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
            return

        # Typing indicator is driven by ChannelManager (send_typing on inbound +
        # every heartbeat beat, stop_typing on the final reply), not started here.
        await self._handle_message(
            sender_id=sender_id,
            chat_id=effective_chat_id,
            text=text,
            media=media if media else None,
            reply_to_text=reply_to_text,
            reply_to_sender=reply_to_sender,
            metadata={"message_id": message_id, "chat_type": chat_type},
        )

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
