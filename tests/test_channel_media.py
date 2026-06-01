"""Tests for inbound media handling across channels and the context builder.

Covers the four fixes:
  1. weixin media extraction: type detection, relative-URL resolution, missing-URL placeholder
  2. InboundEvent.media_items preserves type/mime
  3. ContextBuilder routes images vs files correctly (no more files-as-images)
  4. ContextBuilder downloads remote inbound media to the local cache
"""

from __future__ import annotations

from pathlib import Path
from unittest.mock import AsyncMock

import pytest

from echo_agent.agent.context import ContextBuilder
from echo_agent.bus.events import ContentBlock, ContentType, InboundEvent
from echo_agent.bus.queue import MessageBus
from echo_agent.channels.weixin import WeixinChannel
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
    async def test_non_image_media_not_downloaded(self, tmp_path: Path):
        cb = ContextBuilder(workspace=tmp_path)
        fake_cache = AsyncMock()
        cb._media_cache = fake_cache

        blocks = [
            ContentBlock(type=ContentType.FILE, url="https://cdn/r.pdf", metadata={"name": "r.pdf"}),
            ContentBlock(type=ContentType.VIDEO, url="https://cdn/v.mp4"),
        ]
        out = await cb.resolve_inbound_media(blocks, channel="weixin")

        # 模型无法消费文件/视频字节，不应浪费带宽下载，原始 URL 透传。
        fake_cache.download.assert_not_called()
        assert out[0]["url"] == "https://cdn/r.pdf"
        assert out[0]["name"] == "r.pdf"
        assert out[1]["url"] == "https://cdn/v.mp4"

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
