"""Tests for Weixin inbound message parsing helpers and _process_message.

Covers:
  1. _extract_text: plain text, quoted/ref message, voice transcription fallback
  2. _guess_chat_type: group (room_id) vs direct message
  3. _resolve_media_url / _extract_media_info: image/file/video, missing-URL
     placeholders, aes_key passthrough, CDN-relative joining
  4. _load_sync_buf / _save_sync_buf round-trip and error tolerance
  5. _process_message routing: self-message, dedup, group skip, dm_policy,
     placeholder injection, empty-message skip, happy path
"""

from __future__ import annotations

import asyncio
from pathlib import Path
from unittest.mock import AsyncMock, MagicMock

import pytest

from echo_agent.channels import weixin as wx
from echo_agent.channels.weixin import WeixinChannel
from echo_agent.bus.queue import MessageBus
from echo_agent.config.schema import WeixinChannelConfig


def _make_weixin(tmp_path: Path, **overrides) -> WeixinChannel:
    params = {
        "account_id": "acct@im.bot",
        "token": "acct@im.bot:tok",
        "data_dir": str(tmp_path / "weixin"),
        "cdn_base_url": "https://cdn.weixin.example",
    }
    params.update(overrides)
    cfg = WeixinChannelConfig(**params)
    ch = WeixinChannel(cfg, MessageBus())
    return ch


# ── 1. _extract_text ──────────────────────────────────────────────────────────

class TestExtractText:
    def test_plain_text(self):
        items = [{"type": wx._ITEM_TEXT, "text_item": {"text": "hello"}}]
        assert wx._extract_text(items) == "hello"

    def test_quoted_text_prepends_reference(self):
        items = [{
            "type": wx._ITEM_TEXT,
            "text_item": {"text": "my reply"},
            "ref_msg": {
                "title": "Alice",
                "message_item": {"type": wx._ITEM_TEXT, "text_item": {"text": "original"}},
            },
        }]
        out = wx._extract_text(items)
        assert out == "[引用: Alice | original]\nmy reply"

    def test_quoted_non_text_ref_ignored(self):
        items = [{
            "type": wx._ITEM_TEXT,
            "text_item": {"text": "reply"},
            "ref_msg": {"message_item": {"type": wx._ITEM_IMAGE}},
        }]
        assert wx._extract_text(items) == "reply"

    def test_voice_transcription_fallback(self):
        items = [{"type": wx._ITEM_VOICE, "voice_item": {"text": "spoken words"}}]
        assert wx._extract_text(items) == "spoken words"

    def test_empty_when_no_known_items(self):
        assert wx._extract_text([{"type": 999}]) == ""


# ── 2. _guess_chat_type ─────────────────────────────────────────────────────────

class TestGuessChatType:
    def test_group_from_room_id(self):
        msg = {"room_id": "room@chatroom", "from_user_id": "u@x"}
        assert wx._guess_chat_type(msg, "acct@im.bot") == ("group", "room@chatroom")

    def test_dm_when_no_room(self):
        msg = {"from_user_id": "u@x"}
        assert wx._guess_chat_type(msg, "acct@im.bot") == ("dm", "u@x")


# ── 3. media URL / info ─────────────────────────────────────────────────────────

class TestResolveMediaUrl:
    def test_empty(self, tmp_path):
        assert _make_weixin(tmp_path)._resolve_media_url("  ") == ""

    def test_absolute_kept(self, tmp_path):
        ch = _make_weixin(tmp_path)
        assert ch._resolve_media_url("https://a/b.png") == "https://a/b.png"

    def test_protocol_relative_gets_https(self, tmp_path):
        ch = _make_weixin(tmp_path)
        assert ch._resolve_media_url("//host/x.png") == "https://host/x.png"

    def test_relative_joined_to_cdn(self, tmp_path):
        ch = _make_weixin(tmp_path)
        assert ch._resolve_media_url("/path/x.png") == "https://cdn.weixin.example/path/x.png"

    def test_relative_without_cdn_returned_asis(self, tmp_path):
        ch = _make_weixin(tmp_path, cdn_base_url="")
        assert ch._resolve_media_url("path/x.png") == "path/x.png"


class TestExtractMediaInfo:
    def test_image_with_url_and_aes_key(self, tmp_path):
        ch = _make_weixin(tmp_path)
        item = {
            "type": wx._ITEM_IMAGE,
            "image_item": {"media": {"full_url": "https://a/img.png", "aes_key": "K"}},
        }
        assert ch._extract_media_info(item) == {
            "type": "image", "url": "https://a/img.png", "aes_key": "K",
        }

    def test_image_without_url_returns_placeholder(self, tmp_path):
        ch = _make_weixin(tmp_path)
        item = {"type": wx._ITEM_IMAGE, "image_item": {"media": {}}}
        assert ch._extract_media_info(item) == {"type": "image", "label": "[收到图片]"}

    def test_file_with_url(self, tmp_path):
        ch = _make_weixin(tmp_path)
        item = {
            "type": wx._ITEM_FILE,
            "file_item": {"file_name": "doc.pdf", "media": {"full_url": "https://a/d.pdf"}},
        }
        assert ch._extract_media_info(item) == {
            "type": "file", "url": "https://a/d.pdf", "name": "doc.pdf",
        }

    def test_file_without_url_placeholder_includes_name(self, tmp_path):
        ch = _make_weixin(tmp_path)
        item = {"type": wx._ITEM_FILE, "file_item": {"file_name": "doc.pdf", "media": {}}}
        assert ch._extract_media_info(item) == {"type": "file", "label": "[收到文件: doc.pdf]"}

    def test_video_with_decode_key(self, tmp_path):
        ch = _make_weixin(tmp_path)
        item = {
            "type": wx._ITEM_VIDEO,
            "video_item": {"media": {"full_url": "https://a/v.mp4", "decode_key": "DK"}},
        }
        assert ch._extract_media_info(item) == {
            "type": "video", "url": "https://a/v.mp4", "aes_key": "DK",
        }

    def test_video_without_url_placeholder(self, tmp_path):
        ch = _make_weixin(tmp_path)
        item = {"type": wx._ITEM_VIDEO, "video_item": {"media": {}}}
        assert ch._extract_media_info(item) == {"type": "video", "label": "[收到视频]"}

    def test_unknown_item_returns_none(self, tmp_path):
        ch = _make_weixin(tmp_path)
        assert ch._extract_media_info({"type": wx._ITEM_TEXT}) is None


# ── 4. sync buffer persistence ──────────────────────────────────────────────────

class TestSyncBuf:
    def test_round_trip(self, tmp_path):
        wx._save_sync_buf(tmp_path, "acct", "BUF123")
        assert wx._load_sync_buf(tmp_path, "acct") == "BUF123"

    def test_missing_returns_empty(self, tmp_path):
        assert wx._load_sync_buf(tmp_path, "nope") == ""

    def test_corrupt_file_returns_empty(self, tmp_path):
        (tmp_path / "acct.sync.json").write_text("{not json")
        assert wx._load_sync_buf(tmp_path, "acct") == ""


# ── 5. _process_message routing ─────────────────────────────────────────────────

class TestProcessMessage:
    def _ch(self, tmp_path, **overrides):
        ch = _make_weixin(tmp_path, **overrides)
        ch._handle_message = AsyncMock()
        ch._start_typing = MagicMock()
        return ch

    @pytest.mark.asyncio
    async def test_ignores_own_message(self, tmp_path):
        ch = self._ch(tmp_path)
        await ch._process_message({"from_user_id": "acct@im.bot"})
        ch._handle_message.assert_not_awaited()

    @pytest.mark.asyncio
    async def test_ignores_empty_sender(self, tmp_path):
        ch = self._ch(tmp_path)
        await ch._process_message({"from_user_id": "  "})
        ch._handle_message.assert_not_awaited()

    @pytest.mark.asyncio
    async def test_dedup_skips_repeat(self, tmp_path):
        ch = self._ch(tmp_path)
        msg = {
            "from_user_id": "u@x",
            "message_id": "m1",
            "item_list": [{"type": wx._ITEM_TEXT, "text_item": {"text": "hi"}}],
        }
        await ch._process_message(msg)
        await ch._process_message(msg)
        assert ch._handle_message.await_count == 1

    @pytest.mark.asyncio
    async def test_group_message_skipped(self, tmp_path):
        ch = self._ch(tmp_path)
        await ch._process_message({
            "from_user_id": "u@x", "room_id": "room@chatroom",
            "item_list": [{"type": wx._ITEM_TEXT, "text_item": {"text": "hi"}}],
        })
        ch._handle_message.assert_not_awaited()

    @pytest.mark.asyncio
    async def test_dm_policy_disabled_skips(self, tmp_path):
        ch = self._ch(tmp_path, dm_policy="disabled")
        await ch._process_message({
            "from_user_id": "u@x",
            "item_list": [{"type": wx._ITEM_TEXT, "text_item": {"text": "hi"}}],
        })
        ch._handle_message.assert_not_awaited()

    @pytest.mark.asyncio
    async def test_empty_message_skipped(self, tmp_path):
        ch = self._ch(tmp_path)
        await ch._process_message({"from_user_id": "u@x", "item_list": []})
        ch._handle_message.assert_not_awaited()

    @pytest.mark.asyncio
    async def test_happy_path_starts_typing_and_handles(self, tmp_path):
        ch = self._ch(tmp_path)
        await ch._process_message({
            "from_user_id": "u@x", "message_id": "m1", "context_token": "ctx",
            "item_list": [{"type": wx._ITEM_TEXT, "text_item": {"text": "hello"}}],
        })
        ch._start_typing.assert_called_once_with("u@x")
        ch._handle_message.assert_awaited_once()
        kwargs = ch._handle_message.await_args.kwargs
        assert kwargs["text"] == "hello"
        assert kwargs["chat_id"] == "u@x"

    @pytest.mark.asyncio
    async def test_media_without_url_adds_placeholder_text(self, tmp_path):
        ch = self._ch(tmp_path)
        await ch._process_message({
            "from_user_id": "u@x",
            "item_list": [
                {"type": wx._ITEM_TEXT, "text_item": {"text": "look"}},
                {"type": wx._ITEM_IMAGE, "image_item": {"media": {}}},
            ],
        })
        kwargs = ch._handle_message.await_args.kwargs
        assert "[收到图片]" in kwargs["text"]

    @pytest.mark.asyncio
    async def test_safe_wrapper_swallows_errors(self, tmp_path):
        ch = self._ch(tmp_path)
        ch._handle_message = AsyncMock(side_effect=RuntimeError("boom"))
        # Should not raise.
        await ch._process_message_safe({
            "from_user_id": "u@x",
            "item_list": [{"type": wx._ITEM_TEXT, "text_item": {"text": "hi"}}],
        })


# ── 6. _poll_loop ───────────────────────────────────────────────────────────────

class TestPollLoop:
    def _ch(self, tmp_path):
        ch = _make_weixin(tmp_path)
        ch._poll_session = MagicMock()
        ch._running = True
        ch._spawn_msg_task = MagicMock(side_effect=lambda c: c.close())
        return ch

    @pytest.mark.asyncio
    async def test_processes_messages_then_stops(self, tmp_path, monkeypatch):
        ch = self._ch(tmp_path)
        spawned: list[dict] = []
        ch._spawn_msg_task = MagicMock(side_effect=lambda c: (c.close(), spawned.append(1)))

        async def fake_get_updates(session, *, base_url, token, sync_buf, timeout_ms):
            ch._running = False  # one iteration only
            return {
                "get_updates_buf": "BUF2",
                "msgs": [{"from_user_id": "u@x", "item_list": []}],
                "longpolling_timeout_ms": 1000,
            }

        monkeypatch.setattr(wx, "_get_updates", fake_get_updates)
        await ch._poll_loop()
        assert spawned == [1]  # one message spawned
        # sync_buf persisted
        assert wx._load_sync_buf(ch._data_dir, ch._account_id) == "BUF2"

    @pytest.mark.asyncio
    async def test_session_expired_pauses_and_continues(self, tmp_path, monkeypatch):
        ch = self._ch(tmp_path)
        sleeps: list[float] = []

        async def fake_sleep(d):
            sleeps.append(d)
            ch._running = False  # break after the expired-branch sleep

        async def fake_get_updates(session, **kwargs):
            return {"errcode": wx._SESSION_EXPIRED_ERRCODE}

        monkeypatch.setattr(wx, "_get_updates", fake_get_updates)
        monkeypatch.setattr(wx.asyncio, "sleep", fake_sleep)
        await ch._poll_loop()
        assert sleeps == [600]  # 10-minute pause on session expiry

    @pytest.mark.asyncio
    async def test_error_ret_backs_off(self, tmp_path, monkeypatch):
        ch = self._ch(tmp_path)
        sleeps: list[float] = []

        async def fake_sleep(d):
            sleeps.append(d)
            ch._running = False

        async def fake_get_updates(session, **kwargs):
            return {"ret": 99}

        monkeypatch.setattr(wx, "_get_updates", fake_get_updates)
        monkeypatch.setattr(wx.asyncio, "sleep", fake_sleep)
        await ch._poll_loop()
        assert sleeps == [wx._RETRY_DELAY]

    @pytest.mark.asyncio
    async def test_exception_backs_off(self, tmp_path, monkeypatch):
        ch = self._ch(tmp_path)
        sleeps: list[float] = []

        async def fake_sleep(d):
            sleeps.append(d)
            ch._running = False

        async def fake_get_updates(session, **kwargs):
            raise RuntimeError("network down")

        monkeypatch.setattr(wx, "_get_updates", fake_get_updates)
        monkeypatch.setattr(wx.asyncio, "sleep", fake_sleep)
        await ch._poll_loop()
        assert sleeps == [wx._RETRY_DELAY]

    @pytest.mark.asyncio
    async def test_cancelled_breaks_cleanly(self, tmp_path, monkeypatch):
        ch = self._ch(tmp_path)

        async def fake_get_updates(session, **kwargs):
            raise asyncio.CancelledError()

        monkeypatch.setattr(wx, "_get_updates", fake_get_updates)
        await ch._poll_loop()  # should return without raising

    @pytest.mark.asyncio
    async def test_on_msg_task_done_logs_failure(self, tmp_path):
        ch = _make_weixin(tmp_path)

        async def boom():
            raise RuntimeError("task failed")

        task = asyncio.ensure_future(boom())
        ch._msg_tasks.add(task)
        with pytest.raises(RuntimeError):
            await task
        ch._on_msg_task_done(task)
        assert task not in ch._msg_tasks

