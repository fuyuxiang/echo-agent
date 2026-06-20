"""Tests for inbound media handling across channels and the context builder.

Covers the four fixes:
  1. weixin media extraction: type detection, relative-URL resolution, missing-URL placeholder
  2. InboundEvent.media_items preserves type/mime
  3. ContextBuilder routes images vs files correctly (no more files-as-images)
  4. ContextBuilder downloads remote inbound media to the local cache

Plus multi-turn image persistence:
  5. MediaRef serialization
  6. History image injection via _inject_history_images
  7. TTL filtering, limit capping, skip-if-current logic
  8. Graceful degradation when cache file is gone
  9. Compression strips media_refs
"""

from __future__ import annotations

import time
from pathlib import Path
from unittest.mock import AsyncMock, MagicMock

import pytest

from echo_agent.agent.context import ContextBuilder
from echo_agent.bus.events import ContentBlock, ContentType, InboundEvent
from echo_agent.bus.queue import MessageBus
from echo_agent.channels.weixin import WeixinChannel
from echo_agent.session.media_ref import MediaRef
from echo_agent.config.schema import WeixinChannelConfig


def _make_weixin(tmp_path: Path) -> WeixinChannel:
    cfg = WeixinChannelConfig(
        account_id="acct@im.bot",
        token="acct@im.bot:tok",
        data_dir=str(tmp_path / "weixin"),
    )
    return WeixinChannel(cfg, MessageBus())


# ── 1. weixin extraction ──────────────────────────────────────────────────────

class TestWeixinExtraction:
    def test_image_absolute_url(self, tmp_path: Path):
        ch = _make_weixin(tmp_path)
        item = {"type": 2, "image_item": {"media": {"full_url": "https://cdn/x.jpg"}}}
        assert ch._extract_media_info(item) == {"type": "image", "url": "https://cdn/x.jpg"}

    def test_file_with_name(self, tmp_path: Path):
        ch = _make_weixin(tmp_path)
        item = {"type": 4, "file_item": {"file_name": "report.pdf", "media": {"full_url": "https://cdn/r.pdf"}}}
        out = ch._extract_media_info(item)
        assert out == {"type": "file", "url": "https://cdn/r.pdf", "name": "report.pdf"}

    def test_relative_url_joined_with_cdn_base(self, tmp_path: Path):
        ch = _make_weixin(tmp_path)
        item = {"type": 2, "image_item": {"media": {"full_url": "abc/def.jpg"}}}
        out = ch._extract_media_info(item)
        assert out["url"] == "https://novac2c.cdn.weixin.qq.com/c2c/abc/def.jpg"

    def test_protocol_relative_url(self, tmp_path: Path):
        ch = _make_weixin(tmp_path)
        item = {"type": 2, "image_item": {"media": {"full_url": "//host/x.jpg"}}}
        assert ch._extract_media_info(item)["url"] == "https://host/x.jpg"

    def test_missing_url_returns_placeholder_label(self, tmp_path: Path):
        ch = _make_weixin(tmp_path)
        item = {"type": 4, "file_item": {"file_name": "data.zip", "media": {}}}
        out = ch._extract_media_info(item)
        assert out == {"type": "file", "label": "[收到文件: data.zip]"}

    @pytest.mark.asyncio
    async def test_file_without_url_not_dropped(self, tmp_path: Path, monkeypatch):
        ch = _make_weixin(tmp_path)
        captured = {}

        async def fake_handle(**kwargs):
            captured.update(kwargs)

        monkeypatch.setattr(ch, "_handle_message", fake_handle)
        await ch._process_message({
            "from_user_id": "user-1",
            "message_id": "m1",
            "item_list": [{"type": 4, "file_item": {"file_name": "a.bin", "media": {}}}],
        })
        # 没有文字、没有可下载 URL，过去会被静默丢弃；现在应带占位文本入站。
        assert "收到文件" in captured.get("text", "")

    @pytest.mark.asyncio
    async def test_empty_message_still_dropped(self, tmp_path: Path, monkeypatch):
        ch = _make_weixin(tmp_path)
        called = False

        async def fake_handle(**kwargs):
            nonlocal called
            called = True

        monkeypatch.setattr(ch, "_handle_message", fake_handle)
        await ch._process_message({
            "from_user_id": "user-1",
            "message_id": "m2",
            "item_list": [],
        })
        assert called is False


# ── 2. InboundEvent.media_items ───────────────────────────────────────────────

def test_media_items_preserves_type_and_mime():
    ev = InboundEvent(
        channel="weixin",
        content=[
            ContentBlock(type=ContentType.TEXT, text="hi"),
            ContentBlock(type=ContentType.IMAGE, url="http://i.png"),
            ContentBlock(type=ContentType.FILE, url="http://f.pdf", mime_type="report.pdf"),
        ],
    )
    items = ev.media_items
    assert [b.type for b in items] == [ContentType.IMAGE, ContentType.FILE]
    assert items[1].mime_type == "report.pdf"


# ── 3. ContextBuilder routing ─────────────────────────────────────────────────

class TestContextRouting:
    def test_image_goes_to_image_url(self, tmp_path: Path):
        cb = ContextBuilder(workspace=tmp_path)
        msgs = cb.build_messages(
            history=[], current_message="look",
            media=[{"type": "image", "url": "https://x/i.png"}],
        )
        parts = msgs[-1]["content"]
        assert any(p.get("type") == "image_url" for p in parts)

    def test_file_not_sent_as_image(self, tmp_path: Path):
        cb = ContextBuilder(workspace=tmp_path)
        msgs = cb.build_messages(
            history=[], current_message="read this",
            media=[{"type": "file", "url": "https://x/r.pdf", "name": "r.pdf"}],
        )
        parts = msgs[-1]["content"]
        assert all(p.get("type") != "image_url" for p in parts)
        assert "附件" in parts[0]["text"]
        assert "r.pdf" in parts[0]["text"]

    def test_legacy_bare_url_list_still_image(self, tmp_path: Path):
        cb = ContextBuilder(workspace=tmp_path)
        msgs = cb.build_messages(
            history=[], current_message="x",
            media=["https://x/i.png"],
        )
        parts = msgs[-1]["content"]
        assert parts[1]["type"] == "image_url"
        assert parts[1]["image_url"]["url"] == "https://x/i.png"


# ── 4. ContextBuilder download ────────────────────────────────────────────────

class TestInboundDownload:
    @pytest.mark.asyncio
    async def test_remote_media_downloaded_to_local(self, tmp_path: Path):
        cb = ContextBuilder(workspace=tmp_path)
        fake_cache = AsyncMock()
        fake_cache.download.return_value = tmp_path / "cached.jpg"
        cb._media_cache = fake_cache

        blocks = [ContentBlock(type=ContentType.IMAGE, url="https://cdn/x.jpg")]
        out = await cb.resolve_inbound_media(blocks, channel="weixin")

        fake_cache.download.assert_awaited_once()
        assert out[0]["url"] == str(tmp_path / "cached.jpg")
        assert out[0]["type"] == "image"

    @pytest.mark.asyncio
    async def test_download_failure_falls_back_to_url(self, tmp_path: Path):
        cb = ContextBuilder(workspace=tmp_path)
        fake_cache = AsyncMock()
        fake_cache.download.return_value = None
        cb._media_cache = fake_cache

        blocks = [ContentBlock(type=ContentType.IMAGE, url="https://cdn/x.jpg")]
        out = await cb.resolve_inbound_media(blocks, channel="weixin")

        # 下载失败时回退到原始 URL，消息不丢。
        assert out[0]["url"] == "https://cdn/x.jpg"
        assert out[0]["type"] == "image"

    @pytest.mark.asyncio
    async def test_download_exception_falls_back_to_url(self, tmp_path: Path):
        cb = ContextBuilder(workspace=tmp_path)
        fake_cache = AsyncMock()
        fake_cache.download.side_effect = RuntimeError("boom")
        cb._media_cache = fake_cache

        blocks = [ContentBlock(type=ContentType.IMAGE, url="https://cdn/x.jpg")]
        out = await cb.resolve_inbound_media(blocks, channel="weixin")

        assert out[0]["url"] == "https://cdn/x.jpg"

    @pytest.mark.asyncio
    async def test_file_downloaded_video_audio_not(self, tmp_path: Path):
        cb = ContextBuilder(workspace=tmp_path)
        fake_cache = AsyncMock()
        fake_cache.download.return_value = tmp_path / "r.txt"
        (tmp_path / "r.txt").write_text("doc body text", encoding="utf-8")
        cb._media_cache = fake_cache
        blocks = [
            ContentBlock(type=ContentType.FILE, url="https://cdn/r.txt", metadata={"name": "r.txt"}),
            ContentBlock(type=ContentType.VIDEO, url="https://cdn/v.mp4"),
        ]
        out = await cb.resolve_inbound_media(blocks, channel="weixin")
        # file 进下载队列；video 不进
        assert fake_cache.download.await_count == 1
        assert out[1]["url"] == "https://cdn/v.mp4"

    @pytest.mark.asyncio
    async def test_small_document_text_injected(self, tmp_path: Path):
        cb = ContextBuilder(workspace=tmp_path, doc_max_chars=8000)
        local = tmp_path / "note.txt"
        local.write_text("MEETING_MINUTES_BODY", encoding="utf-8")
        fake_cache = AsyncMock()
        fake_cache.download.return_value = local
        cb._media_cache = fake_cache
        blocks = [ContentBlock(type=ContentType.FILE, url="https://cdn/note.txt",
                               metadata={"name": "note.txt"})]
        resolved = await cb.resolve_inbound_media(blocks, channel="weixin")
        msgs = cb.build_messages(history=[], current_message="总结一下", media=resolved)
        text = msgs[-1]["content"][0]["text"]
        assert "MEETING_MINUTES_BODY" in text

    @pytest.mark.asyncio
    async def test_large_document_summarized_with_hint(self, tmp_path: Path):
        cb = ContextBuilder(workspace=tmp_path, doc_max_chars=50)
        local = tmp_path / "big.txt"
        local.write_text("y" * 5000, encoding="utf-8")
        fake_cache = AsyncMock()
        fake_cache.download.return_value = local
        cb._media_cache = fake_cache
        blocks = [ContentBlock(type=ContentType.FILE, url="https://cdn/big.txt",
                               metadata={"name": "big.txt"})]
        resolved = await cb.resolve_inbound_media(blocks, channel="weixin")
        msgs = cb.build_messages(history=[], current_message="读这个", media=resolved)
        text = msgs[-1]["content"][0]["text"]
        assert "read_document" in text
        assert len([c for c in text if c == "y"]) <= 60  # 截断生效

    @pytest.mark.asyncio
    async def test_file_with_aes_key_decrypted(self, tmp_path: Path):
        cb = ContextBuilder(workspace=tmp_path)
        local = tmp_path / "enc.txt"
        local.write_text("ENC", encoding="utf-8")
        fake_cache = AsyncMock()
        fake_cache.download.return_value = local
        cb._media_cache = fake_cache
        called = {}
        def spy(path, key):
            called["key"] = key
            return path
        cb._decrypt_media_file = spy
        blocks = [ContentBlock(type=ContentType.FILE, url="https://cdn/enc.docx",
                               metadata={"name": "enc.docx", "aes_key": "QUJDREVGR0hJSktMTU5P"})]
        await cb.resolve_inbound_media(blocks, channel="weixin")
        assert called.get("key") == "QUJDREVGR0hJSktMTU5P"

    @pytest.mark.asyncio
    async def test_extract_failure_falls_back_to_note(self, tmp_path: Path):
        cb = ContextBuilder(workspace=tmp_path)
        local = tmp_path / "broken.docx"
        local.write_bytes(b"not a real docx")
        fake_cache = AsyncMock()
        fake_cache.download.return_value = local
        cb._media_cache = fake_cache
        blocks = [ContentBlock(type=ContentType.FILE, url="https://cdn/broken.docx",
                               metadata={"name": "broken.docx"})]
        resolved = await cb.resolve_inbound_media(blocks, channel="weixin")
        msgs = cb.build_messages(history=[], current_message="x", media=resolved)
        text = msgs[-1]["content"][0]["text"]
        assert "[附件]" in text and "broken.docx" in text

    @pytest.mark.asyncio
    async def test_local_path_not_downloaded(self, tmp_path: Path):
        cb = ContextBuilder(workspace=tmp_path)
        fake_cache = AsyncMock()
        cb._media_cache = fake_cache

        local = str(tmp_path / "already.png")
        blocks = [ContentBlock(type=ContentType.IMAGE, url=local)]
        out = await cb.resolve_inbound_media(blocks, channel="weixin")

        fake_cache.download.assert_not_called()
        assert out[0]["url"] == local

    def test_missing_image_falls_back_to_note(self, tmp_path: Path):
        # 图片本地路径不存在（缓存被清理）时，降级为文本附件而非静默丢弃。
        cb = ContextBuilder(workspace=tmp_path)
        msgs = cb.build_messages(
            history=[], current_message="see this",
            media=[{"type": "image", "url": str(tmp_path / "gone.png"), "name": "gone.png"}],
        )
        parts = msgs[-1]["content"]
        assert all(p.get("type") != "image_url" for p in parts)
        assert "附件" in parts[0]["text"]
        assert "gone.png" in parts[0]["text"]


# ── 5. MediaRef serialization ────────────────────────────────────────────────

class TestMediaRef:
    def test_round_trip(self):
        ref = MediaRef(
            cache_path="/tmp/abc.jpg",
            original_url="https://cdn/x.jpg",
            mime_type="image/jpeg",
            timestamp=1000.0,
            aes_key="key123",
        )
        d = ref.to_dict()
        assert d["cache_path"] == "/tmp/abc.jpg"
        assert d["aes_key"] == "key123"
        restored = MediaRef.from_dict(d)
        assert restored.cache_path == ref.cache_path
        assert restored.aes_key == ref.aes_key

    def test_aes_key_omitted_when_empty(self):
        ref = MediaRef(cache_path="/tmp/x.jpg", original_url="", mime_type="image/jpeg")
        d = ref.to_dict()
        assert "aes_key" not in d

    def test_from_dict_defaults(self):
        ref = MediaRef.from_dict({})
        assert ref.cache_path == ""
        assert ref.timestamp == 0.0


# ── 6. History image injection ───────────────────────────────────────────────

def _make_image(tmp_path: Path, name: str = "test.jpg") -> Path:
    """Create a minimal JPEG file for testing."""
    img = tmp_path / name
    img.write_bytes(
        b"\xff\xd8\xff\xe0\x00\x10JFIF\x00\x01\x01\x00\x00\x01\x00\x01\x00\x00"
        b"\xff\xd9"
    )
    return img


class TestHistoryImageInjection:
    def test_history_image_injected(self, tmp_path: Path):
        img = _make_image(tmp_path)
        cb = ContextBuilder(workspace=tmp_path)
        history = [
            {"role": "user", "content": "look at this", "media_refs": [
                {"cache_path": str(img), "original_url": "", "mime_type": "image/jpeg",
                 "timestamp": time.time()}
            ]},
            {"role": "assistant", "content": "nice image"},
        ]
        msgs = cb.build_messages(history=history, current_message="what is in the image?")
        user_with_image = msgs[0]
        assert isinstance(user_with_image["content"], list)
        assert any(p.get("type") == "image_url" for p in user_with_image["content"])

    def test_history_image_expired_by_ttl(self, tmp_path: Path):
        img = _make_image(tmp_path)
        cb = ContextBuilder(workspace=tmp_path)
        history = [
            {"role": "user", "content": "old image", "media_refs": [
                {"cache_path": str(img), "original_url": "", "mime_type": "image/jpeg",
                 "timestamp": time.time() - 3600}
            ]},
        ]
        msgs = cb.build_messages(
            history=history, current_message="what?",
            history_image_ttl_minutes=30,
        )
        user_msg = msgs[1]
        assert isinstance(user_msg["content"], str)

    def test_history_image_limit(self, tmp_path: Path):
        imgs = [_make_image(tmp_path, f"img{i}.jpg") for i in range(5)]
        cb = ContextBuilder(workspace=tmp_path)
        now = time.time()
        history = [
            {"role": "user", "content": f"image {i}", "media_refs": [
                {"cache_path": str(imgs[i]), "original_url": "", "mime_type": "image/jpeg",
                 "timestamp": now}
            ]}
            for i in range(5)
        ]
        msgs = cb.build_messages(
            history=history, current_message="what?",
            history_image_limit=2,
        )
        image_count = sum(
            1 for m in msgs
            if isinstance(m.get("content"), list)
            and any(p.get("type") == "image_url" for p in m["content"])
        )
        assert image_count == 2

    def test_skip_when_current_has_image(self, tmp_path: Path):
        img = _make_image(tmp_path, "hist.jpg")
        cur_img = _make_image(tmp_path, "cur.jpg")
        cb = ContextBuilder(workspace=tmp_path)
        history = [
            {"role": "user", "content": "old", "media_refs": [
                {"cache_path": str(img), "original_url": "", "mime_type": "image/jpeg",
                 "timestamp": time.time()}
            ]},
        ]
        msgs = cb.build_messages(
            history=history, current_message="new image",
            media=[{"type": "image", "url": str(cur_img)}],
            history_image_skip_if_current=True,
        )
        hist_msg = msgs[0]
        assert isinstance(hist_msg["content"], str)

    def test_cache_file_gone_degrades_to_text(self, tmp_path: Path):
        cb = ContextBuilder(workspace=tmp_path)
        history = [
            {"role": "user", "content": "img here", "media_refs": [
                {"cache_path": str(tmp_path / "gone.jpg"), "original_url": "",
                 "mime_type": "image/jpeg", "timestamp": time.time()}
            ]},
        ]
        msgs = cb.build_messages(history=history, current_message="what?")
        enriched = msgs[0]
        assert isinstance(enriched["content"], list)
        assert any("过期" in p.get("text", "") for p in enriched["content"])

    def test_no_media_refs_unchanged(self, tmp_path: Path):
        cb = ContextBuilder(workspace=tmp_path)
        history = [
            {"role": "user", "content": "hello"},
            {"role": "assistant", "content": "hi"},
        ]
        msgs = cb.build_messages(history=history, current_message="how are you?")
        assert isinstance(msgs[0]["content"], str)
        assert msgs[0]["content"] == "hello"

    def test_history_image_has_text_annotation(self, tmp_path: Path):
        img = _make_image(tmp_path)
        cb = ContextBuilder(workspace=tmp_path)
        history = [
            {"role": "user", "content": "look", "media_refs": [
                {"cache_path": str(img), "original_url": "", "mime_type": "image/jpeg",
                 "timestamp": time.time() - 120}
            ]},
        ]
        msgs = cb.build_messages(history=history, current_message="what?")
        parts = msgs[0]["content"]
        text_parts = [p.get("text", "") for p in parts if p.get("type") == "text"]
        assert any("历史图片" in t for t in text_parts)

    def test_original_history_not_mutated(self, tmp_path: Path):
        img = _make_image(tmp_path)
        cb = ContextBuilder(workspace=tmp_path)
        history = [
            {"role": "user", "content": "look", "media_refs": [
                {"cache_path": str(img), "original_url": "", "mime_type": "image/jpeg",
                 "timestamp": time.time()}
            ]},
        ]
        cb.build_messages(history=history, current_message="what?")
        assert isinstance(history[0]["content"], str)


# ── 7. format_utils image_url → Anthropic conversion ────────────────────────

class TestFormatUtilsImageConversion:
    def test_data_url_converted_to_anthropic_base64(self):
        from echo_agent.models.providers.format_utils import openai_to_anthropic_messages
        messages = [
            {"role": "user", "content": [
                {"type": "text", "text": "look"},
                {"type": "image_url", "image_url": {"url": "data:image/jpeg;base64,/9j/4AAQ"}},
            ]},
        ]
        _, converted = openai_to_anthropic_messages(messages)
        blocks = converted[0]["content"]
        img_block = [b for b in blocks if b.get("type") == "image"][0]
        assert img_block["source"]["type"] == "base64"
        assert img_block["source"]["media_type"] == "image/jpeg"
        assert img_block["source"]["data"] == "/9j/4AAQ"

    def test_http_url_converted_to_anthropic_url(self):
        from echo_agent.models.providers.format_utils import openai_to_anthropic_messages
        messages = [
            {"role": "user", "content": [
                {"type": "image_url", "image_url": {"url": "https://cdn/img.jpg"}},
            ]},
        ]
        _, converted = openai_to_anthropic_messages(messages)
        blocks = converted[0]["content"]
        img_block = [b for b in blocks if b.get("type") == "image"][0]
        assert img_block["source"]["type"] == "url"

    def test_text_only_list_passes_through(self):
        from echo_agent.models.providers.format_utils import openai_to_anthropic_messages
        messages = [
            {"role": "user", "content": [{"type": "text", "text": "hello"}]},
        ]
        _, converted = openai_to_anthropic_messages(messages)
        assert converted[0]["content"][0]["text"] == "hello"

    def test_system_with_list_content(self):
        from echo_agent.models.providers.format_utils import openai_to_anthropic_messages
        messages = [
            {"role": "system", "content": [{"type": "text", "text": "sys prompt"}]},
            {"role": "user", "content": "hi"},
        ]
        system, _ = openai_to_anthropic_messages(messages)
        assert system[0]["text"] == "sys prompt"

    def test_assistant_with_list_content(self):
        from echo_agent.models.providers.format_utils import openai_to_anthropic_messages
        messages = [
            {"role": "user", "content": "x"},
            {"role": "assistant", "content": [
                {"type": "text", "text": "I see"},
                {"type": "image_url", "image_url": {"url": "data:image/png;base64,abc"}},
            ]},
        ]
        _, converted = openai_to_anthropic_messages(messages)
        asst = [m for m in converted if m["role"] == "assistant"][0]
        assert any(b.get("type") == "image" for b in asst["content"])


# ── 8. Consolidator multimodal safety ────────────────────────────────────────

class TestConsolidatorMultimodal:
    def test_format_messages_extracts_text_from_list(self):
        from echo_agent.memory.consolidator import MemoryConsolidator
        messages = [
            {"role": "user", "content": [
                {"type": "text", "text": "describe this"},
                {"type": "image_url", "image_url": {"url": "data:image/jpeg;base64,huge"}},
            ], "timestamp": "2025-01-01T00:00"},
        ]
        result = MemoryConsolidator._format_messages(messages)
        assert "describe this" in result
        assert "[image]" in result
        assert "base64" not in result

    def test_format_messages_string_content_unchanged(self):
        from echo_agent.memory.consolidator import MemoryConsolidator
        messages = [
            {"role": "user", "content": "hello", "timestamp": "2025-01-01T00:00"},
        ]
        result = MemoryConsolidator._format_messages(messages)
        assert "hello" in result


# ── 9. Compression strips media_refs ─────────────────────────────────────────

class TestCompressionMediaRefs:
    def test_media_refs_stripped_before_compression(self):
        messages = [
            {"role": "user", "content": "look at this", "media_refs": [
                {"cache_path": "/tmp/x.jpg", "timestamp": time.time()}
            ]},
            {"role": "assistant", "content": "nice"},
        ]
        working = list(messages)
        for msg in working:
            if "media_refs" in msg:
                del msg["media_refs"]
        assert "media_refs" not in working[0]
        assert working[0]["content"] == "look at this"


# ── 10. Cross-channel media download (_resolve_media_to_cache) ──────────────

class TestResolveMediaToCache:
    @pytest.mark.asyncio
    async def test_fetch_success_caches_to_disk(self, tmp_path: Path):
        from echo_agent.channels.base import BaseChannel

        class DummyChannel(BaseChannel):
            name = "dummy"
            async def start(self): ...
            async def stop(self): ...
            async def send(self, event): ...

        ch = DummyChannel(type("C", (), {"allow_from": []})(), MessageBus())
        ch._media_cache_root = tmp_path

        fetch = AsyncMock(return_value=b"\xff\xd8\xff\xe0JFIF_DATA")
        result = await ch._resolve_media_to_cache("test_id", "testplatform", fetch, suffix=".jpg")

        assert result is not None
        assert Path(result).exists()
        assert Path(result).read_bytes() == b"\xff\xd8\xff\xe0JFIF_DATA"
        fetch.assert_awaited_once()

    @pytest.mark.asyncio
    async def test_cache_hit_skips_fetch(self, tmp_path: Path):
        from echo_agent.channels.base import BaseChannel
        import hashlib

        class DummyChannel(BaseChannel):
            name = "dummy"
            async def start(self): ...
            async def stop(self): ...
            async def send(self, event): ...

        ch = DummyChannel(type("C", (), {"allow_from": []})(), MessageBus())
        ch._media_cache_root = tmp_path

        url_hash = hashlib.sha256(b"cached_id").hexdigest()[:16]
        cache_dir = tmp_path / "testplatform"
        cache_dir.mkdir(parents=True)
        existing = cache_dir / f"{url_hash}.jpg"
        existing.write_bytes(b"cached_data")

        fetch = AsyncMock(return_value=b"new_data")
        result = await ch._resolve_media_to_cache("cached_id", "testplatform", fetch, suffix=".jpg")

        assert result == str(existing)
        fetch.assert_not_awaited()

    @pytest.mark.asyncio
    async def test_fetch_failure_returns_none(self, tmp_path: Path):
        from echo_agent.channels.base import BaseChannel

        class DummyChannel(BaseChannel):
            name = "dummy"
            async def start(self): ...
            async def stop(self): ...
            async def send(self, event): ...

        ch = DummyChannel(type("C", (), {"allow_from": []})(), MessageBus())
        ch._media_cache_root = tmp_path

        fetch = AsyncMock(side_effect=RuntimeError("network error"))
        result = await ch._resolve_media_to_cache("fail_id", "testplatform", fetch)
        assert result is None

    @pytest.mark.asyncio
    async def test_empty_bytes_returns_none(self, tmp_path: Path):
        from echo_agent.channels.base import BaseChannel

        class DummyChannel(BaseChannel):
            name = "dummy"
            async def start(self): ...
            async def stop(self): ...
            async def send(self, event): ...

        ch = DummyChannel(type("C", (), {"allow_from": []})(), MessageBus())
        ch._media_cache_root = tmp_path

        fetch = AsyncMock(return_value=b"")
        result = await ch._resolve_media_to_cache("empty_id", "testplatform", fetch)
        assert result is None


# ── 11. Channel-specific media download integration ─────────────────────────

class TestTelegramMediaDownload:
    @pytest.mark.asyncio
    async def test_photo_resolved_to_local_path(self, tmp_path: Path, monkeypatch):
        from echo_agent.channels.telegram import TelegramChannel
        from echo_agent.config.schema import TelegramChannelConfig

        cfg = TelegramChannelConfig(token="123:ABCTOKEN")
        ch = TelegramChannel(cfg, MessageBus())
        ch._media_cache_root = tmp_path

        ch._api = AsyncMock(return_value={"file_path": "photos/file_42.jpg"})
        mock_resp = AsyncMock()
        mock_resp.status = 200
        mock_resp.read = AsyncMock(return_value=b"\xff\xd8JPEG_DATA")
        mock_ctx = MagicMock()
        mock_ctx.__aenter__ = AsyncMock(return_value=mock_resp)
        mock_ctx.__aexit__ = AsyncMock(return_value=False)
        mock_session = MagicMock()
        mock_session.get.return_value = mock_ctx
        ch._session = mock_session

        result = await ch._download_telegram_file("AgACAgIAA_FILE_ID")
        assert result is not None
        assert Path(result).exists()
        ch._api.assert_awaited_once_with("getFile", json={"file_id": "AgACAgIAA_FILE_ID"})


class TestFeishuMediaDownload:
    @pytest.mark.asyncio
    async def test_image_key_resolved(self, tmp_path: Path):
        from echo_agent.channels.feishu import FeishuChannel
        from echo_agent.config.schema import FeishuChannelConfig

        cfg = FeishuChannelConfig(app_id="cli_a", app_secret="sec")
        ch = FeishuChannel(cfg, MessageBus())
        ch._media_cache_root = tmp_path
        ch._tenant_token = "t-valid-token"
        ch._token_expires = time.time() + 7200

        mock_resp = AsyncMock()
        mock_resp.status = 200
        mock_resp.read = AsyncMock(return_value=b"\x89PNG_IMAGE_DATA")
        mock_ctx = MagicMock()
        mock_ctx.__aenter__ = AsyncMock(return_value=mock_resp)
        mock_ctx.__aexit__ = AsyncMock(return_value=False)
        mock_session = MagicMock()
        mock_session.get.return_value = mock_ctx
        ch._session = mock_session

        result = await ch._download_feishu_image("img_v2_abc123", "om_msg_001")
        assert result is not None
        assert Path(result).exists()


class TestWhatsAppMediaDownload:
    @pytest.mark.asyncio
    async def test_media_id_two_step_download(self, tmp_path: Path):
        from echo_agent.channels.whatsapp import WhatsAppChannel
        from echo_agent.config.schema import WhatsAppChannelConfig

        cfg = WhatsAppChannelConfig(
            access_token="EAAx", phone_number_id="123", verify_token="vt"
        )
        ch = WhatsAppChannel(cfg, MessageBus())
        ch._media_cache_root = tmp_path

        call_count = 0
        def mock_get(url, **kwargs):
            nonlocal call_count
            call_count += 1
            resp = AsyncMock()
            resp.status = 200
            if call_count == 1:
                resp.json = AsyncMock(return_value={"url": "https://cdn.whatsapp.net/file.enc"})
            else:
                resp.read = AsyncMock(return_value=b"IMAGE_BYTES")
            ctx = MagicMock()
            ctx.__aenter__ = AsyncMock(return_value=resp)
            ctx.__aexit__ = AsyncMock(return_value=False)
            return ctx

        mock_session = MagicMock()
        mock_session.get = mock_get
        ch._session = mock_session

        result = await ch._download_whatsapp_media("1234567890")
        assert result is not None
        assert call_count == 2


class TestMatrixMediaDownload:
    @pytest.mark.asyncio
    async def test_mxc_converted_to_http(self, tmp_path: Path):
        from echo_agent.channels.matrix import MatrixChannel
        from echo_agent.config.schema import MatrixChannelConfig

        cfg = MatrixChannelConfig(
            homeserver="https://matrix.example.com",
            user_id="@bot:example.com",
            access_token="syt_token",
        )
        ch = MatrixChannel(cfg, MessageBus())
        ch._media_cache_root = tmp_path

        mock_resp = AsyncMock()
        mock_resp.status = 200
        mock_resp.read = AsyncMock(return_value=b"MATRIX_IMAGE")
        mock_ctx = MagicMock()
        mock_ctx.__aenter__ = AsyncMock(return_value=mock_resp)
        mock_ctx.__aexit__ = AsyncMock(return_value=False)
        mock_session = MagicMock()
        mock_session.get.return_value = mock_ctx
        ch._session = mock_session

        result = await ch._download_matrix_media("mxc://example.com/AbCdEfGhIjK")
        assert result is not None
        assert Path(result).exists()

    @pytest.mark.asyncio
    async def test_invalid_mxc_returns_none(self, tmp_path: Path):
        from echo_agent.channels.matrix import MatrixChannel
        from echo_agent.config.schema import MatrixChannelConfig

        cfg = MatrixChannelConfig(
            homeserver="https://matrix.example.com",
            user_id="@bot:example.com",
            access_token="syt_token",
        )
        ch = MatrixChannel(cfg, MessageBus())
        ch._media_cache_root = tmp_path

        result = await ch._download_matrix_media("not_an_mxc_url")
        assert result is None


class TestSlackMediaDownload:
    @pytest.mark.asyncio
    async def test_url_private_downloaded_with_auth(self, tmp_path: Path):
        from echo_agent.channels.slack import SlackChannel
        from echo_agent.config.schema import SlackChannelConfig

        cfg = SlackChannelConfig(bot_token="xoxb-test", app_token="xapp-test")
        ch = SlackChannel(cfg, MessageBus())
        ch._media_cache_root = tmp_path

        mock_resp = AsyncMock()
        mock_resp.status = 200
        mock_resp.headers = {"Content-Type": "image/png"}
        mock_resp.read = AsyncMock(return_value=b"SLACK_IMAGE")
        mock_ctx = MagicMock()
        mock_ctx.__aenter__ = AsyncMock(return_value=mock_resp)
        mock_ctx.__aexit__ = AsyncMock(return_value=False)
        mock_session = MagicMock()
        mock_session.get.return_value = mock_ctx
        ch._session = mock_session

        result = await ch._download_slack_file("https://files.slack.com/files-pri/T0/F0/image.png")
        assert result is not None
        assert Path(result).exists()

    @pytest.mark.asyncio
    async def test_html_login_page_rejected(self, tmp_path: Path):
        from echo_agent.channels.slack import SlackChannel
        from echo_agent.config.schema import SlackChannelConfig

        cfg = SlackChannelConfig(bot_token="xoxb-test", app_token="xapp-test")
        ch = SlackChannel(cfg, MessageBus())
        ch._media_cache_root = tmp_path

        mock_resp = AsyncMock()
        mock_resp.status = 200
        mock_resp.headers = {"Content-Type": "text/html"}
        mock_resp.read = AsyncMock(return_value=b"<html>login</html>")
        mock_ctx = MagicMock()
        mock_ctx.__aenter__ = AsyncMock(return_value=mock_resp)
        mock_ctx.__aexit__ = AsyncMock(return_value=False)
        mock_session = MagicMock()
        mock_session.get.return_value = mock_ctx
        ch._session = mock_session

        result = await ch._download_slack_file("https://files.slack.com/files-pri/T0/F0/image.png")
        assert result is None
