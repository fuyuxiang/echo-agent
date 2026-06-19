"""Pure-logic unit tests for IM channel adapters — precision-strike strategy.

This module deliberately tests ONLY stable, high-value pure logic:
  - signature / token verification (security-critical)
  - encryption / decryption round-trips (security-critical)
  - inbound webhook/event payload parsing into internal messages
  - outbound message formatting and media-source classification
  - @mention parsing, dedup, and CLI streaming bookkeeping

It does NOT exercise real aiohttp round-trips, websocket loops, or polling
loops — mocking those would only test the mock. Where a channel's parsing
method is async and ends in ``_handle_message``, we replace that sink with an
AsyncMock and assert on the captured kwargs, which keeps the test focused on
the parsing logic alone.
"""

from __future__ import annotations

import base64
import hashlib
import json
from unittest.mock import AsyncMock, MagicMock

import pytest


# ── Shared helpers ────────────────────────────────────────────────────────────


def _mock_bus():
    bus = MagicMock()
    bus.publish_inbound = AsyncMock(return_value=True)
    bus.publish_outbound = AsyncMock(return_value=True)
    bus.subscribe_outbound = MagicMock()
    return bus


# ══════════════════════════════════════════════════════════════════════════════
# Feishu — AES-CBC decrypt round-trip, token gate, event parsing
# ══════════════════════════════════════════════════════════════════════════════


def _feishu_channel(encryption_key: str = "", verification_token: str = "vtoken"):
    from echo_agent.channels.feishu import FeishuChannel

    cfg = MagicMock()
    cfg.app_id = "app"
    cfg.app_secret = "secret"
    cfg.verification_token = verification_token
    cfg.encryption_key = encryption_key
    cfg.webhook_path = "/feishu"
    cfg.host = "0.0.0.0"
    cfg.port = 8083
    cfg.allow_from = []
    return FeishuChannel(cfg, _mock_bus())


def _feishu_encrypt(plaintext: str, key_str: str) -> str:
    """Reproduce Feishu's encrypt scheme: AES-256-CBC, PKCS7, iv prefixed,
    keyed by sha256(encryption_key). Mirrors FeishuChannel._decrypt."""
    from cryptography.hazmat.primitives.ciphers import Cipher, algorithms, modes

    key = hashlib.sha256(key_str.encode()).digest()
    iv = b"\x00" * 16
    data = plaintext.encode("utf-8")
    pad_len = 16 - (len(data) % 16)
    data += bytes([pad_len]) * pad_len
    cipher = Cipher(algorithms.AES(key), modes.CBC(iv))
    enc = cipher.encryptor()
    ct = enc.update(data) + enc.finalize()
    return base64.b64encode(iv + ct).decode()


class TestFeishuCrypto:
    def test_decrypt_round_trip(self):
        """A payload encrypted with Feishu's scheme must decrypt back to the
        exact original dict. This guards the AES-CBC + PKCS7 + sha256-key path."""
        ch = _feishu_channel(encryption_key="my-encrypt-key")
        original = {"type": "url_verification", "challenge": "abc123", "n": 42}
        token = _feishu_encrypt(json.dumps(original), "my-encrypt-key")

        assert ch._decrypt(token) == original

    def test_decrypt_wrong_key_returns_none(self):
        """Decrypting with the wrong key must fail closed (None), never leak
        garbage as a parsed event."""
        ch = _feishu_channel(encryption_key="right-key")
        token = _feishu_encrypt(json.dumps({"x": 1}), "wrong-key")

        assert ch._decrypt(token) is None

    def test_decrypt_garbage_returns_none(self):
        ch = _feishu_channel(encryption_key="k")
        assert ch._decrypt("not-valid-base64!!!") is None


class TestFeishuEventParsing:
    @pytest.mark.asyncio
    async def test_v2_text_message_parsed(self):
        ch = _feishu_channel()
        ch._handle_message = AsyncMock()
        event = {
            "sender": {"sender_id": {"open_id": "ou_123"}, "sender_type": "user"},
            "message": {
                "chat_id": "oc_chat",
                "message_type": "text",
                "message_id": "om_1",
                "chat_type": "group",
                "content": json.dumps({"text": "hello feishu"}),
            },
        }
        await ch._on_message(event)

        ch._handle_message.assert_called_once()
        kw = ch._handle_message.call_args.kwargs
        assert kw["text"] == "hello feishu"
        assert kw["sender_id"] == "ou_123"
        assert kw["chat_id"] == "oc_chat"
        assert kw["reply_to_id"] == "om_1"
        assert kw["metadata"]["receive_id_type"] == "chat_id"

    @pytest.mark.asyncio
    async def test_v2_app_sender_ignored(self):
        """Messages from the bot itself (sender_type == app) must not loop back."""
        ch = _feishu_channel()
        ch._handle_message = AsyncMock()
        await ch._on_message({"sender": {"sender_type": "app"}, "message": {}})
        ch._handle_message.assert_not_called()

    @pytest.mark.asyncio
    async def test_v2_malformed_text_content_falls_back_to_raw(self):
        ch = _feishu_channel()
        ch._handle_message = AsyncMock()
        event = {
            "sender": {"sender_id": {"open_id": "ou"}, "sender_type": "user"},
            "message": {
                "chat_id": "oc",
                "message_type": "text",
                "message_id": "m",
                "chat_type": "p2p",
                "content": "not-json",
            },
        }
        await ch._on_message(event)
        kw = ch._handle_message.call_args.kwargs
        assert kw["text"] == "not-json"

    @pytest.mark.asyncio
    async def test_v1_message_parsed_prefers_text_without_at_bot(self):
        ch = _feishu_channel()
        ch._handle_message = AsyncMock()
        await ch._on_message_v1({
            "open_id": "ou_x",
            "open_chat_id": "oc_x",
            "text_without_at_bot": "clean text",
            "text": "@bot clean text",
            "chat_type": "group",
        })
        kw = ch._handle_message.call_args.kwargs
        assert kw["text"] == "clean text"


# ══════════════════════════════════════════════════════════════════════════════
# Weixin (iLink) — AES-128-ECB decrypt, key parsing, item extraction
# ══════════════════════════════════════════════════════════════════════════════


class TestWeixinCrypto:
    def test_aes128_ecb_decrypt_round_trip(self):
        from cryptography.hazmat.primitives.ciphers import Cipher, algorithms, modes
        from echo_agent.channels.weixin import _aes128_ecb_decrypt, _pkcs7_pad

        key = b"0123456789abcdef"
        plaintext = b"hello weixin media payload"
        padded = _pkcs7_pad(plaintext)
        enc = Cipher(algorithms.AES(key), modes.ECB()).encryptor()
        ct = enc.update(padded) + enc.finalize()

        assert _aes128_ecb_decrypt(ct, key) == plaintext

    def test_parse_aes_key_16_raw_bytes(self):
        from echo_agent.channels.weixin import _parse_aes_key

        raw = b"sixteen-byte-key"  # exactly 16 bytes
        assert _parse_aes_key(base64.b64encode(raw).decode()) == raw

    def test_parse_aes_key_32_hex_chars(self):
        """A 32-char base64-decoded hex string must be interpreted as hex → 16
        raw bytes, not used directly as a 32-byte key."""
        from echo_agent.channels.weixin import _parse_aes_key

        hex_text = "00112233445566778899aabbccddeeff"  # 32 hex chars
        encoded = base64.b64encode(hex_text.encode()).decode()
        assert _parse_aes_key(encoded) == bytes.fromhex(hex_text)

    def test_parse_aes_key_invalid_length_raises(self):
        from echo_agent.channels.weixin import _parse_aes_key

        with pytest.raises(ValueError):
            _parse_aes_key(base64.b64encode(b"short").decode())


class TestWeixinItemExtraction:
    def test_extract_text_plain(self):
        from echo_agent.channels.weixin import _ITEM_TEXT, _extract_text

        items = [{"type": _ITEM_TEXT, "text_item": {"text": "hi there"}}]
        assert _extract_text(items) == "hi there"

    def test_extract_text_with_quoted_reference(self):
        from echo_agent.channels.weixin import _ITEM_TEXT, _extract_text

        items = [{
            "type": _ITEM_TEXT,
            "text_item": {"text": "my reply"},
            "ref_msg": {
                "title": "Alice",
                "message_item": {"type": _ITEM_TEXT, "text_item": {"text": "original"}},
            },
        }]
        result = _extract_text(items)
        assert "引用" in result
        assert "Alice" in result
        assert "original" in result
        assert "my reply" in result

    def test_extract_text_falls_back_to_voice(self):
        from echo_agent.channels.weixin import _ITEM_VOICE, _extract_text

        items = [{"type": _ITEM_VOICE, "voice_item": {"text": "transcribed"}}]
        assert _extract_text(items) == "transcribed"

    def test_extract_text_empty(self):
        from echo_agent.channels.weixin import _extract_text

        assert _extract_text([]) == ""

    def test_guess_chat_type_group_when_room_id(self):
        from echo_agent.channels.weixin import _guess_chat_type

        kind, cid = _guess_chat_type({"room_id": "room42", "from_user_id": "u"}, "acct")
        assert kind == "group"
        assert cid == "room42"

    def test_guess_chat_type_dm_without_room(self):
        from echo_agent.channels.weixin import _guess_chat_type

        kind, cid = _guess_chat_type({"from_user_id": "user9"}, "acct")
        assert kind == "dm"
        assert cid == "user9"


class TestWeixinDeduplicator:
    def test_marks_repeat_as_duplicate(self):
        from echo_agent.channels.weixin import _MessageDeduplicator

        dedup = _MessageDeduplicator(ttl=300)
        assert dedup.is_duplicate("a") is False
        assert dedup.is_duplicate("a") is True
        assert dedup.is_duplicate("b") is False

    def test_evicts_after_ttl(self, monkeypatch):
        from echo_agent.channels import weixin

        dedup = weixin._MessageDeduplicator(ttl=10)
        monkeypatch.setattr(weixin.time, "time", lambda: 1000.0)
        dedup.is_duplicate("old")
        monkeypatch.setattr(weixin.time, "time", lambda: 1011.0)
        # Stale entry swept → not seen as duplicate, and old key dropped.
        assert dedup.is_duplicate("new") is False
        assert "old" not in dedup._seen


# ══════════════════════════════════════════════════════════════════════════════
# QQBot — attachment extraction, bare-URL splitting, @mention stripping
# ══════════════════════════════════════════════════════════════════════════════


class TestQQBotAttachments:
    def test_extract_attachments_classifies_by_content_type(self):
        from echo_agent.channels.qqbot import _extract_attachments

        d = {"attachments": [
            {"url": "http://a/img.png", "content_type": "image/png"},
            {"url": "http://a/clip.mp4", "content_type": "video/mp4"},
            {"url": "http://a/voice.amr", "content_type": "audio/amr"},
            {"url": "http://a/doc.pdf", "content_type": "application/pdf"},
        ]}
        media = _extract_attachments(d)
        kinds = [m["type"] for m in media]
        assert kinds == ["image", "video", "audio", "file"]

    def test_extract_attachments_prepends_https_scheme(self):
        """QQ sometimes returns scheme-less URLs; they must be normalized so
        downstream HTTP fetches don't fail."""
        from echo_agent.channels.qqbot import _extract_attachments

        media = _extract_attachments({"attachments": [
            {"url": "cdn.qq.com/x.png", "content_type": "image/png"},
        ]})
        assert media[0]["url"] == "https://cdn.qq.com/x.png"

    def test_extract_attachments_skips_empty_url(self):
        from echo_agent.channels.qqbot import _extract_attachments

        assert _extract_attachments({"attachments": [{"content_type": "image/png"}]}) == []

    def test_extract_attachments_none(self):
        from echo_agent.channels.qqbot import _extract_attachments

        assert _extract_attachments({}) == []


class TestQQBotMentionStripping:
    def test_at_mention_regex_strips_leading_mention(self):
        from echo_agent.channels.qqbot import _AT_MENTION_RE

        cleaned = _AT_MENTION_RE.sub("", "<@!12345> hello bot").strip()
        assert cleaned == "hello bot"

    def test_at_mention_regex_strips_multiple(self):
        from echo_agent.channels.qqbot import _AT_MENTION_RE

        cleaned = _AT_MENTION_RE.sub("", "<@111> <@!222> ping").strip()
        assert cleaned == "ping"


class TestQQBotFileUrlDetection:
    def test_detect_file_urls_splits_text_and_media(self):
        from echo_agent.channels.qqbot import QQBotChannel
        from echo_agent.channels.qqbot_media import SendQueueItem

        queue = [SendQueueItem(kind="text", content="see this https://x.com/report.pdf now")]
        result = QQBotChannel._detect_file_urls_in_text(queue)
        kinds = [(i.kind, i.content) for i in result]
        assert ("text", "see this") in kinds
        assert any(k == "file" and "report.pdf" in c for k, c in kinds)
        assert ("text", "now") in kinds

    def test_detect_file_urls_leaves_plain_text_untouched(self):
        from echo_agent.channels.qqbot import QQBotChannel
        from echo_agent.channels.qqbot_media import SendQueueItem

        queue = [SendQueueItem(kind="text", content="just a normal sentence")]
        result = QQBotChannel._detect_file_urls_in_text(queue)
        assert len(result) == 1
        assert result[0].content == "just a normal sentence"

    def test_detect_file_urls_passes_through_non_text_items(self):
        from echo_agent.channels.qqbot import QQBotChannel
        from echo_agent.channels.qqbot_media import SendQueueItem

        queue = [SendQueueItem(kind="image", content="http://x/a.png")]
        result = QQBotChannel._detect_file_urls_in_text(queue)
        assert result == queue


class TestQQBotContentTypeMapping:
    def test_content_type_to_kind(self):
        from echo_agent.bus.events import ContentType
        from echo_agent.channels.qqbot import QQBotChannel

        assert QQBotChannel._content_type_to_kind(ContentType.IMAGE, "", "") == "image"
        assert QQBotChannel._content_type_to_kind(ContentType.AUDIO, "", "") == "voice"
        assert QQBotChannel._content_type_to_kind(ContentType.VIDEO, "", "") == "video"
        # FILE delegates to detect_media_kind on the URL/mime.
        assert QQBotChannel._content_type_to_kind(ContentType.FILE, "x.mp3", "") == "voice"
        assert QQBotChannel._content_type_to_kind(ContentType.FILE, "x.bin", "") == "file"


class TestQQBotChatTypeMap:
    def _channel(self):
        from echo_agent.bus.queue import MessageBus
        from echo_agent.channels.qqbot import QQBotChannel

        cfg = MagicMock()
        cfg.app_id = "x"
        cfg.app_secret = "y"
        cfg.sandbox = False
        cfg.markdown_support = False
        cfg.media_enabled = False
        cfg.media_parse_tags = False
        cfg.media_max_file_size_mb = 10
        cfg.media_upload_cache_size = 4
        return QQBotChannel(cfg, MessageBus())

    def test_set_chat_type_evicts_lru_beyond_cap(self):
        ch = self._channel()
        ch._max_chat_type_entries = 3
        for i in range(5):
            ch._set_chat_type(f"chat{i}", "group")
        assert len(ch._chat_type_map) == 3
        # Oldest two evicted, newest retained.
        assert "chat0" not in ch._chat_type_map
        assert "chat4" in ch._chat_type_map

    def test_next_msg_seq_is_monotonic(self):
        ch = self._channel()
        assert [ch._next_msg_seq() for _ in range(3)] == [1, 2, 3]


# ══════════════════════════════════════════════════════════════════════════════
# qqbot_media — UploadCache TTL/LRU, tilde expansion, tag normalization edges
# ══════════════════════════════════════════════════════════════════════════════


class TestUploadCache:
    def test_hit_returns_file_info(self):
        from echo_agent.channels.qqbot_media import UploadCache

        cache = UploadCache(max_size=4)
        cache.set("h1", "group", "g1", 1, "finfo", "uuid", ttl=3600)
        assert cache.get("h1", "group", "g1", 1) == "finfo"

    def test_miss_returns_none(self):
        from echo_agent.channels.qqbot_media import UploadCache

        assert UploadCache().get("nope", "group", "g1", 1) is None

    def test_key_is_scoped_per_target_and_type(self):
        """The same content hash for a different target or file_type must NOT
        collide — otherwise media leaks across chats."""
        from echo_agent.channels.qqbot_media import UploadCache

        cache = UploadCache()
        cache.set("h", "group", "g1", 1, "finfo_g1", "u", ttl=3600)
        assert cache.get("h", "group", "g2", 1) is None
        assert cache.get("h", "group", "g1", 2) is None

    def test_expired_entry_evicted_on_get(self, monkeypatch):
        from echo_agent.channels import qqbot_media

        cache = qqbot_media.UploadCache()
        monkeypatch.setattr(qqbot_media.time, "time", lambda: 1000.0)
        cache.set("h", "group", "g1", 1, "finfo", "u", ttl=120)
        # ttl-60 grace → expires_at == 1060. Jump beyond it.
        monkeypatch.setattr(qqbot_media.time, "time", lambda: 2000.0)
        assert cache.get("h", "group", "g1", 1) is None

    def test_lru_eviction_when_full(self):
        from echo_agent.channels.qqbot_media import UploadCache

        cache = UploadCache(max_size=2)
        cache.set("h1", "group", "g", 1, "f1", "u1", ttl=3600)
        cache.set("h2", "group", "g", 1, "f2", "u2", ttl=3600)
        # Touch h1 so h2 becomes the least-recently-used.
        cache.get("h1", "group", "g", 1)
        cache.set("h3", "group", "g", 1, "f3", "u3", ttl=3600)
        assert cache.get("h2", "group", "g", 1) is None
        assert cache.get("h1", "group", "g", 1) == "f1"
        assert cache.get("h3", "group", "g", 1) == "f3"

    def test_compute_hash_stable_across_str_and_bytes(self):
        from echo_agent.channels.qqbot_media import UploadCache

        assert UploadCache.compute_hash("abc") == UploadCache.compute_hash(b"abc")
        assert UploadCache.compute_hash("abc") == hashlib.md5(b"abc").hexdigest()


class TestQQBotMediaTagParsing:
    def test_normalize_expands_tilde_home(self, monkeypatch):
        from echo_agent.channels.qqbot_media import normalize_media_tags

        monkeypatch.setenv("HOME", "/home/tester")
        result = normalize_media_tags('<qqimg src="~/pics/a.png" />')
        assert "/home/tester/pics/a.png" in result

    def test_parse_send_queue_fullwidth_brackets(self):
        """Models sometimes emit full-width brackets; the parser normalizes
        them so media still gets recognized."""
        from echo_agent.channels.qqbot_media import parse_send_queue

        items = parse_send_queue("＜qqimg＞pic.png＜/qqimg＞")
        assert any(i.kind == "image" and "pic.png" in i.content for i in items)

    def test_parse_send_queue_alias_and_text_interleaved(self):
        from echo_agent.channels.qqbot_media import parse_send_queue

        items = parse_send_queue("before <photo>x.png</photo> after")
        kinds = [i.kind for i in items]
        assert kinds.count("text") == 2
        assert "image" in kinds

    def test_resolve_tag_name_unknown_defaults_to_image(self):
        from echo_agent.channels.qqbot_media import _resolve_tag_name

        assert _resolve_tag_name("totally-unknown") == "qqimg"

    def test_media_kind_to_file_type(self):
        from echo_agent.channels.qqbot_media import MediaFileType, media_kind_to_file_type

        assert media_kind_to_file_type("image") == MediaFileType.IMAGE
        assert media_kind_to_file_type("voice") == MediaFileType.VOICE
        assert media_kind_to_file_type("video") == MediaFileType.VIDEO
        assert media_kind_to_file_type("file") == MediaFileType.FILE
        assert media_kind_to_file_type("???") == MediaFileType.FILE

    def test_source_classifiers(self):
        from echo_agent.channels.qqbot_media import (
            is_data_source,
            is_http_source,
            is_local_path,
        )

        assert is_http_source("https://x/a.png") is True
        assert is_data_source("data:image/png;base64,AAAA") is True
        assert is_local_path("/tmp/a.png") is True
        assert is_local_path("https://x/a.png") is False

    def test_image_mime_for_extension(self):
        from echo_agent.channels.qqbot_media import image_mime_for

        assert image_mime_for("a.jpg") == "image/jpeg"
        assert image_mime_for("a.webp") == "image/webp"
        # Unknown extension defaults to png.
        assert image_mime_for("a.unknown") == "image/png"


# ══════════════════════════════════════════════════════════════════════════════
# Telegram — @mention detection (group gating logic)
# ══════════════════════════════════════════════════════════════════════════════


def _telegram_channel(group_policy="mention"):
    from echo_agent.channels.telegram import TelegramChannel

    cfg = MagicMock()
    cfg.token = "t"
    cfg.proxy = None
    cfg.group_policy = group_policy
    cfg.allow_from = []
    ch = TelegramChannel(cfg, _mock_bus())
    ch._bot_id = "999"
    ch._bot_username = "echo_bot"
    return ch


class TestTelegramMention:
    def test_mention_by_username_substring(self):
        ch = _telegram_channel()
        assert ch._is_mentioned({}, "hey @echo_bot what's up") is True

    def test_mention_by_entity_case_insensitive(self):
        ch = _telegram_channel()
        text = "@Echo_Bot hello"
        msg = {"entities": [{"type": "mention", "offset": 0, "length": 9}]}
        assert ch._is_mentioned(msg, text) is True

    def test_mention_by_reply_to_bot(self):
        ch = _telegram_channel()
        msg = {"reply_to_message": {"from": {"id": 999}}}
        assert ch._is_mentioned(msg, "no explicit mention") is True

    def test_not_mentioned(self):
        ch = _telegram_channel()
        assert ch._is_mentioned({}, "talking to nobody") is False

    def test_chunk_text_uses_shared_splitter(self):
        from echo_agent.channels.telegram import TelegramChannel
        from echo_agent.utils.text import split_message

        text = "a\n" + ("b" * 5000)
        assert TelegramChannel._chunk_text(text, 4096) == split_message(text, 4096)

    def test_send_result_success_and_failure(self):
        from echo_agent.channels.telegram import TelegramChannel

        ok = TelegramChannel._send_result({"message_id": 7}, "err")
        assert ok.success is True
        assert ok.message_id == "7"
        fail = TelegramChannel._send_result(None, "boom", fallback_message_id="f")
        assert fail.success is False
        assert fail.message_id == "f"


# ══════════════════════════════════════════════════════════════════════════════
# Discord — message parsing, bot filtering, mention gating
# ══════════════════════════════════════════════════════════════════════════════


def _discord_channel(group_policy="mention"):
    from echo_agent.channels.discord import DiscordChannel

    cfg = MagicMock()
    cfg.token = "t"
    cfg.group_policy = group_policy
    cfg.allow_from = []
    ch = DiscordChannel(cfg, _mock_bus())
    ch._bot_id = "555"
    return ch


class TestDiscordMessageParsing:
    @pytest.mark.asyncio
    async def test_ignores_bot_authors(self):
        ch = _discord_channel()
        ch._handle_message = AsyncMock()
        await ch._on_message({"author": {"bot": True, "id": "1"}, "content": "hi"})
        ch._handle_message.assert_not_called()

    @pytest.mark.asyncio
    async def test_dm_message_parsed(self):
        ch = _discord_channel()
        ch._handle_message = AsyncMock()
        await ch._on_message({
            "author": {"id": "42"},
            "channel_id": "chan1",
            "content": "hello discord",
            "id": "msg1",
        })
        kw = ch._handle_message.call_args.kwargs
        assert kw["text"] == "hello discord"
        assert kw["chat_id"] == "chan1"
        assert kw["reply_to_id"] == "msg1"

    @pytest.mark.asyncio
    async def test_guild_mention_required_and_stripped(self):
        ch = _discord_channel(group_policy="mention")
        ch._handle_message = AsyncMock()
        await ch._on_message({
            "author": {"id": "42"},
            "channel_id": "c",
            "guild_id": "g1",
            "content": "<@555> do the thing",
            "id": "m",
        })
        kw = ch._handle_message.call_args.kwargs
        assert kw["text"] == "do the thing"

    @pytest.mark.asyncio
    async def test_guild_unmentioned_dropped(self):
        ch = _discord_channel(group_policy="mention")
        ch._handle_message = AsyncMock()
        await ch._on_message({
            "author": {"id": "42"},
            "channel_id": "c",
            "guild_id": "g1",
            "content": "just chatting",
            "id": "m",
        })
        ch._handle_message.assert_not_called()

    @pytest.mark.asyncio
    async def test_attachments_classified(self):
        ch = _discord_channel(group_policy="all")
        ch._handle_message = AsyncMock()
        await ch._on_message({
            "author": {"id": "42"},
            "channel_id": "c",
            "content": "look",
            "id": "m",
            "attachments": [
                {"url": "http://x/a.png", "content_type": "image/png"},
                {"url": "http://x/a.zip", "content_type": "application/zip"},
            ],
        })
        media = ch._handle_message.call_args.kwargs["media"]
        assert media[0]["type"] == "image"
        assert media[1]["type"] == "file"


# ══════════════════════════════════════════════════════════════════════════════
# Slack — event parsing, subtype/bot filtering
# ══════════════════════════════════════════════════════════════════════════════


def _slack_channel():
    from echo_agent.channels.slack import SlackChannel

    cfg = MagicMock()
    cfg.bot_token = "xoxb"
    cfg.app_token = "xapp"
    cfg.allow_from = []
    ch = SlackChannel(cfg, _mock_bus())
    ch._bot_id = "UBOT"
    return ch


class TestSlackEventParsing:
    @pytest.mark.asyncio
    async def test_text_message_parsed_with_thread(self):
        ch = _slack_channel()
        ch._handle_message = AsyncMock()
        await ch._on_event({
            "type": "message",
            "user": "U123",
            "channel": "C1",
            "text": "hello slack",
            "ts": "1700000000.000100",
        })
        kw = ch._handle_message.call_args.kwargs
        assert kw["text"] == "hello slack"
        assert kw["chat_id"] == "C1"
        assert kw["metadata"]["thread_ts"] == "1700000000.000100"

    @pytest.mark.asyncio
    async def test_bot_message_ignored(self):
        ch = _slack_channel()
        ch._handle_message = AsyncMock()
        await ch._on_event({"type": "message", "bot_id": "B1", "text": "x"})
        ch._handle_message.assert_not_called()

    @pytest.mark.asyncio
    async def test_subtype_message_ignored(self):
        ch = _slack_channel()
        ch._handle_message = AsyncMock()
        await ch._on_event({"type": "message", "subtype": "message_changed", "text": "x"})
        ch._handle_message.assert_not_called()

    @pytest.mark.asyncio
    async def test_non_message_event_ignored(self):
        ch = _slack_channel()
        ch._handle_message = AsyncMock()
        await ch._on_event({"type": "reaction_added"})
        ch._handle_message.assert_not_called()


# ══════════════════════════════════════════════════════════════════════════════
# Matrix — timeline event parsing, self-echo & allow-room filtering
# ══════════════════════════════════════════════════════════════════════════════


def _matrix_channel(allow_rooms=None):
    from echo_agent.channels.matrix import MatrixChannel

    cfg = MagicMock()
    cfg.homeserver = "https://hs.example/"
    cfg.user_id = "@bot:example"
    cfg.access_token = "tok"
    cfg.allow_rooms = allow_rooms
    cfg.allow_from = []
    return MatrixChannel(cfg, _mock_bus())


class TestMatrixEventParsing:
    @pytest.mark.asyncio
    async def test_text_message_parsed(self):
        ch = _matrix_channel()
        ch._handle_message = AsyncMock()
        await ch._on_event("!room:example", {
            "type": "m.room.message",
            "sender": "@alice:example",
            "event_id": "$evt1",
            "content": {"msgtype": "m.text", "body": "hello matrix"},
        })
        kw = ch._handle_message.call_args.kwargs
        assert kw["text"] == "hello matrix"
        assert kw["chat_id"] == "!room:example"
        assert kw["reply_to_id"] == "$evt1"

    @pytest.mark.asyncio
    async def test_self_message_ignored(self):
        ch = _matrix_channel()
        ch._handle_message = AsyncMock()
        await ch._on_event("!room:example", {
            "type": "m.room.message",
            "sender": "@bot:example",
            "content": {"msgtype": "m.text", "body": "echo"},
        })
        ch._handle_message.assert_not_called()

    @pytest.mark.asyncio
    async def test_non_message_event_ignored(self):
        ch = _matrix_channel()
        ch._handle_message = AsyncMock()
        await ch._on_event("!r:e", {"type": "m.room.member", "sender": "@a:e"})
        ch._handle_message.assert_not_called()

    def test_homeserver_trailing_slash_stripped(self):
        ch = _matrix_channel()
        assert ch._homeserver == "https://hs.example"

    def test_allow_rooms_set_built(self):
        ch = _matrix_channel(allow_rooms=["!a:e", "!b:e"])
        assert ch._allow_rooms == {"!a:e", "!b:e"}

    def test_allow_rooms_none_when_unset(self):
        ch = _matrix_channel(allow_rooms=None)
        assert ch._allow_rooms is None


# ══════════════════════════════════════════════════════════════════════════════
# WeCom — sha1 signature verification + XML webhook parsing
# ══════════════════════════════════════════════════════════════════════════════


def _wecom_channel(token="mytoken"):
    from echo_agent.channels.wecom import WeComChannel

    cfg = MagicMock()
    cfg.corp_id = "corp"
    cfg.agent_id = "1000001"
    cfg.secret = "sec"
    cfg.token = token
    cfg.webhook_path = "/wecom"
    cfg.host = "0.0.0.0"
    cfg.port = 8084
    cfg.allow_from = []
    return WeComChannel(cfg, _mock_bus())


class TestWeComSignature:
    def test_signature_valid(self):
        ch = _wecom_channel(token="tok")
        ts, nonce = "1700000000", "n123"
        expected = hashlib.sha1("".join(sorted(["tok", ts, nonce])).encode()).hexdigest()
        assert ch._check_signature(expected, ts, nonce) is True

    def test_signature_rejects_tampered(self):
        ch = _wecom_channel(token="tok")
        assert ch._check_signature("deadbeef", "1700000000", "n123") is False

    def test_signature_order_independent(self):
        """Signature is built from a sorted tuple, so arg order at the call
        site must not matter as long as the same three values are used."""
        ch = _wecom_channel(token="tok")
        ts, nonce = "ts-val", "nonce-val"
        sig = hashlib.sha1("".join(sorted(["tok", ts, nonce])).encode()).hexdigest()
        assert ch._check_signature(sig, ts, nonce) is True


# ══════════════════════════════════════════════════════════════════════════════
# WhatsApp — Cloud API message parsing
# ══════════════════════════════════════════════════════════════════════════════


def _whatsapp_channel():
    from echo_agent.channels.whatsapp import WhatsAppChannel

    cfg = MagicMock()
    cfg.verify_token = "vt"
    cfg.access_token = "at"
    cfg.phone_number_id = "phone1"
    cfg.webhook_path = "/wa"
    cfg.host = "0.0.0.0"
    cfg.port = 8081
    cfg.allow_from = []
    return WhatsAppChannel(cfg, _mock_bus())


class TestWhatsAppParsing:
    @pytest.mark.asyncio
    async def test_text_message_parsed(self):
        ch = _whatsapp_channel()
        ch._handle_message = AsyncMock()
        await ch._process_message(
            {"from": "+15551234", "type": "text", "text": {"body": "hi wa"}},
            {},
        )
        kw = ch._handle_message.call_args.kwargs
        assert kw["text"] == "hi wa"
        assert kw["sender_id"] == "+15551234"
        assert kw["chat_id"] == "+15551234"
        assert kw["metadata"]["message_type"] == "text"

    @pytest.mark.asyncio
    async def test_empty_message_dropped(self):
        ch = _whatsapp_channel()
        ch._handle_message = AsyncMock()
        await ch._process_message({"from": "+1", "type": "text", "text": {"body": ""}}, {})
        ch._handle_message.assert_not_called()


# ══════════════════════════════════════════════════════════════════════════════
# CLI — streaming dedup / remainder bookkeeping (no real stdin)
# ══════════════════════════════════════════════════════════════════════════════


def _cli_channel():
    from echo_agent.channels.cli import CLIChannel

    cfg = MagicMock()
    cfg.allow_from = []
    return CLIChannel(cfg, _mock_bus())


def _stream_event(text, *, eid="e1", final=False):
    from echo_agent.bus.events import OutboundEvent

    event = OutboundEvent.text_reply(channel="cli", chat_id="cli", text=text)
    event.metadata = {"_token_stream": True, "_inbound_event_id": eid}
    event.message_kind = "final" if final else "streaming"
    return event


def _feed(ch, text, *, eid="e1", final=False):
    """Drive CLIChannel._send_stream(event, text) with matching args."""
    ev = _stream_event(text, eid=eid, final=final)
    return ch._send_stream(ev, text)


class TestCLIStreaming:
    def test_streaming_chunks_accumulate(self, capsys):
        ch = _cli_channel()
        _feed(ch, "Hello ", eid="e1")
        _feed(ch, "world", eid="e1")
        assert ch._stream_printed["e1"] == "Hello world"

    def test_final_prints_only_remainder(self, capsys):
        """The final FULL-text event must only print the part not yet streamed,
        so a print-only channel doesn't duplicate the reply."""
        ch = _cli_channel()
        _feed(ch, "Hello ", eid="e1")
        capsys.readouterr()  # clear
        _feed(ch, "Hello world", eid="e1", final=True)
        out = capsys.readouterr().out
        assert "world" in out
        assert "Hello world" not in out.replace("world", "")  # no full re-print of prefix
        # Stream bookkeeping cleared after final.
        assert "e1" not in ch._stream_printed

    def test_final_divergent_reprints_full(self, capsys):
        ch = _cli_channel()
        _feed(ch, "partial chunk", eid="e1")
        capsys.readouterr()
        _feed(ch, "completely different", eid="e1", final=True)
        out = capsys.readouterr().out
        assert "完整回复" in out
        assert "completely different" in out

    def test_final_without_prior_stream_prints_plainly(self, capsys):
        ch = _cli_channel()
        _feed(ch, "standalone final", eid="new", final=True)
        out = capsys.readouterr().out
        assert "standalone final" in out

    def test_stream_entry_cap_evicts_oldest(self):
        ch = _cli_channel()
        ch._max_stream_entries = 4
        for i in range(10):
            _feed(ch, "x", eid=f"e{i}")
        assert len(ch._stream_printed) <= 4

