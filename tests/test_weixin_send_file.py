"""Tests for outbound file/image sending on the Weixin channel and send_file tool.

Covers:
  1. encryption / padding correctness (AES-128-ECB round-trip, padded size)
  2. _send_file orchestration: getuploadurl → CDN upload → sendmessage, with
     correct image_item / file_item payloads and base64(hex) aes_key
  3. send() routing: image block → image item, file block → file attachment,
     plain text unaffected
  4. SendFileTool: path-traversal rejection, happy-path OutboundEvent emission
"""

from __future__ import annotations

from pathlib import Path
from unittest.mock import AsyncMock, MagicMock

import pytest

from echo_agent.bus.events import ContentBlock, ContentType, OutboundEvent
from echo_agent.bus.queue import MessageBus
from echo_agent.channels import weixin as wx
from echo_agent.channels.weixin import WeixinChannel
from echo_agent.config.schema import WeixinChannelConfig


def _make_weixin(tmp_path: Path) -> WeixinChannel:
    cfg = WeixinChannelConfig(
        account_id="acct@im.bot",
        token="acct@im.bot:tok",
        data_dir=str(tmp_path / "weixin"),
    )
    ch = WeixinChannel(cfg, MessageBus())
    ch._send_session = MagicMock()  # truthy; real calls are monkeypatched
    return ch


# ── 1. encryption / padding ───────────────────────────────────────────────────

class TestCrypto:
    def test_padded_size_is_multiple_of_16(self):
        for n in (0, 1, 15, 16, 17, 100):
            assert wx._aes_padded_size(n) % 16 == 0
            assert wx._aes_padded_size(n) >= n

    def test_padded_size_adds_full_block_when_aligned(self):
        # PKCS#7 always adds 1..16 bytes, so an already-aligned input grows.
        assert wx._aes_padded_size(16) == 32

    def test_ecb_round_trip(self):
        key = b"0123456789abcdef"
        plaintext = b"hello weixin file payload \x00\x01\x02"
        ciphertext = wx._aes128_ecb_encrypt(plaintext, key)
        assert len(ciphertext) % 16 == 0
        assert ciphertext != plaintext
        assert wx._aes128_ecb_decrypt(ciphertext, key) == plaintext


# ── 2. _send_file orchestration ───────────────────────────────────────────────

class TestSendFileOrchestration:
    @pytest.fixture
    def _patched(self, monkeypatch):
        calls: dict[str, dict] = {}

        async def fake_get_upload_url(session, **kwargs):
            calls["upload_url"] = kwargs
            return {"upload_full_url": "https://novac2c.cdn.weixin.qq.com/c2c/upload?x=1"}

        async def fake_upload_ciphertext(session, *, ciphertext, upload_url):
            calls["upload"] = {"size": len(ciphertext), "url": upload_url}
            return "ENCRYPTED_PARAM_TOKEN"

        async def fake_api_post(session, *, base_url, endpoint, payload, token, timeout_ms):
            calls.setdefault("sendmessage", []).append({"endpoint": endpoint, "payload": payload})
            return {"errcode": 0}

        monkeypatch.setattr(wx, "_get_upload_url", fake_get_upload_url)
        monkeypatch.setattr(wx, "_upload_ciphertext", fake_upload_ciphertext)
        monkeypatch.setattr(wx, "_api_post", fake_api_post)
        return calls

    @pytest.mark.asyncio
    async def test_send_image_builds_image_item(self, tmp_path, _patched):
        ch = _make_weixin(tmp_path)
        f = tmp_path / "pic.png"
        f.write_bytes(b"\x89PNG\r\n fake image bytes")
        res = await ch._send_file("user@im", str(f), as_image=True)
        assert res.success
        # getuploadurl received hex aes_key and padded filesize
        up = _patched["upload_url"]
        assert up["media_type"] == wx._MEDIA_IMAGE
        assert len(up["aeskey_hex"]) == 32  # 16-byte key as hex
        assert up["filesize"] % 16 == 0
        # sendmessage carries an image_item with base64(hex) aes_key
        item = _patched["sendmessage"][-1]["payload"]["msg"]["item_list"][0]
        assert item["type"] == wx._ITEM_IMAGE
        assert "image_item" in item
        media = item["image_item"]["media"]
        assert media["encrypt_query_param"] == "ENCRYPTED_PARAM_TOKEN"
        assert media["encrypt_type"] == 1
        # aes_key must be base64(hex_string), i.e. decodes to 32 hex chars
        import base64
        assert len(base64.b64decode(media["aes_key"])) == 32

    @pytest.mark.asyncio
    async def test_send_file_builds_file_item(self, tmp_path, _patched):
        ch = _make_weixin(tmp_path)
        f = tmp_path / "report.pdf"
        f.write_bytes(b"%PDF-1.7 ...")
        res = await ch._send_file("user@im", str(f), as_image=False)
        assert res.success
        assert _patched["upload_url"]["media_type"] == wx._MEDIA_FILE
        item = _patched["sendmessage"][-1]["payload"]["msg"]["item_list"][0]
        assert item["type"] == wx._ITEM_FILE
        assert item["file_item"]["file_name"] == "report.pdf"
        assert item["file_item"]["len"] == str(len(b"%PDF-1.7 ..."))

    @pytest.mark.asyncio
    async def test_send_voice_builds_voice_item(self, tmp_path, _patched):
        ch = _make_weixin(tmp_path)
        f = tmp_path / "clip.silk"
        f.write_bytes(b"\x02#!SILK_V3 fake silk bytes")
        res = await ch._send_file("user@im", str(f), as_voice=True, voice_ms=1234)
        assert res.success
        assert _patched["upload_url"]["media_type"] == wx._MEDIA_VOICE
        item = _patched["sendmessage"][-1]["payload"]["msg"]["item_list"][0]
        assert item["type"] == wx._ITEM_VOICE
        vi = item["voice_item"]
        assert vi["encode_type"] == 6
        assert vi["sample_rate"] == 24000
        assert vi["bits_per_sample"] == 16
        assert vi["playtime"] == 1234
        assert vi["media"]["encrypt_type"] == 1

    @pytest.mark.asyncio
    async def test_send_file_propagates_sendmessage_error(self, tmp_path, monkeypatch):
        ch = _make_weixin(tmp_path)
        f = tmp_path / "a.bin"
        f.write_bytes(b"x")

        async def ok_upload_url(session, **kwargs):
            return {"upload_full_url": "https://novac2c.cdn.weixin.qq.com/c2c/upload"}

        async def ok_upload(session, *, ciphertext, upload_url):
            return "tok"

        async def err_api_post(session, **kwargs):
            return {"errcode": 5, "errmsg": "boom"}

        monkeypatch.setattr(wx, "_get_upload_url", ok_upload_url)
        monkeypatch.setattr(wx, "_upload_ciphertext", ok_upload)
        monkeypatch.setattr(wx, "_api_post", err_api_post)
        res = await ch._send_file("user@im", str(f), as_image=False)
        assert not res.success
        assert "errcode=5" in res.error


# ── 3. send() routing ──────────────────────────────────────────────────────────

class TestSendRouting:
    @pytest.mark.asyncio
    async def test_text_only_event_uses_send_text(self, tmp_path):
        ch = _make_weixin(tmp_path)
        ch._send_text = AsyncMock(return_value=wx.SendResult(success=True))
        ch.send_file = AsyncMock()
        ev = OutboundEvent.text_reply(channel="weixin", chat_id="u", text="hi")
        res = await ch.send(ev)
        assert res.success
        ch._send_text.assert_awaited_once()
        ch.send_file.assert_not_awaited()

    @pytest.mark.asyncio
    async def test_image_block_routes_to_send_file_as_image(self, tmp_path):
        ch = _make_weixin(tmp_path)
        ch._send_text = AsyncMock(return_value=wx.SendResult(success=True))
        ch.send_file = AsyncMock(return_value=wx.SendResult(success=True))
        ev = OutboundEvent(
            channel="weixin", chat_id="u",
            content=[ContentBlock(type=ContentType.IMAGE, url="/tmp/x.png")],
        )
        res = await ch.send(ev)
        assert res.success
        ch.send_file.assert_awaited_once()
        assert ch.send_file.await_args.kwargs["as_image"] is True

    @pytest.mark.asyncio
    async def test_file_block_routes_as_attachment(self, tmp_path):
        ch = _make_weixin(tmp_path)
        ch._send_text = AsyncMock(return_value=wx.SendResult(success=True))
        ch.send_file = AsyncMock(return_value=wx.SendResult(success=True))
        ev = OutboundEvent(
            channel="weixin", chat_id="u",
            content=[
                ContentBlock(type=ContentType.TEXT, text="here you go"),
                ContentBlock(type=ContentType.FILE, url="/tmp/r.pdf"),
            ],
        )
        res = await ch.send(ev)
        assert res.success
        ch._send_text.assert_awaited_once()
        ch.send_file.assert_awaited_once()
        assert ch.send_file.await_args.kwargs["as_image"] is False

    @pytest.mark.asyncio
    async def test_non_final_skipped(self, tmp_path):
        ch = _make_weixin(tmp_path)
        ev = OutboundEvent.text_reply(channel="weixin", chat_id="u", text="partial")
        ev.is_final = False
        res = await ch.send(ev)
        assert res.skipped


# ── 4. SendFileTool ────────────────────────────────────────────────────────────

class TestSendFileTool:
    @pytest.mark.asyncio
    async def test_rejects_missing_file(self, tmp_path):
        from echo_agent.agent.tools.send_file import SendFileTool

        published: list = []
        tool = SendFileTool(str(tmp_path), restrict=True, publish_fn=lambda e: published.append(e))
        res = await tool.execute({"channel": "weixin", "chat_id": "u", "file_path": "nope.txt"})
        assert not res.success
        assert not published

    @pytest.mark.asyncio
    async def test_rejects_outside_workspace_when_restricted(self, tmp_path):
        from echo_agent.agent.tools.send_file import SendFileTool

        published: list = []
        tool = SendFileTool(str(tmp_path / "ws"), restrict=True, publish_fn=lambda e: published.append(e))
        (tmp_path / "ws").mkdir()
        outside = tmp_path / "secret.txt"
        outside.write_text("x")
        res = await tool.execute({"channel": "weixin", "chat_id": "u", "file_path": str(outside)})
        assert not res.success
        assert not published

    @pytest.mark.asyncio
    async def test_happy_path_emits_file_block(self, tmp_path):
        from echo_agent.agent.tools.send_file import SendFileTool

        published: list[OutboundEvent] = []

        async def _pub(e):
            published.append(e)

        ws = tmp_path / "ws"
        ws.mkdir()
        f = ws / "doc.pdf"
        f.write_text("data")
        tool = SendFileTool(str(ws), restrict=True, publish_fn=_pub)
        res = await tool.execute({
            "channel": "weixin", "chat_id": "u", "file_path": str(f), "caption": "see attached",
        })
        assert res.success
        assert len(published) == 1
        ev = published[0]
        kinds = [(b.type, b.text or b.url) for b in ev.content]
        assert (ContentType.TEXT, "see attached") in kinds
        assert any(b.type == ContentType.FILE and b.url.endswith("doc.pdf") for b in ev.content)

    @pytest.mark.asyncio
    async def test_infers_image_from_extension(self, tmp_path):
        from echo_agent.agent.tools.send_file import SendFileTool

        published: list[OutboundEvent] = []

        async def _pub(e):
            published.append(e)

        ws = tmp_path / "ws"
        ws.mkdir()
        f = ws / "photo.png"
        f.write_bytes(b"\x89PNG")
        tool = SendFileTool(str(ws), restrict=True, publish_fn=_pub)
        res = await tool.execute({"channel": "weixin", "chat_id": "u", "file_path": str(f)})
        assert res.success
        assert any(b.type == ContentType.IMAGE for b in published[0].content)


class TestSendVoiceFallback:
    @pytest.mark.asyncio
    async def test_voice_encode_failure_falls_back_to_file(self, tmp_path, monkeypatch):
        ch = _make_weixin(tmp_path)
        src = tmp_path / "a.mp3"
        src.write_bytes(b"ID3 fake mp3")

        async def boom(_src):
            raise RuntimeError("ffmpeg missing")
        monkeypatch.setattr(wx, "encode_to_silk", boom)

        captured = {}
        async def fake_send_file(chat_id, path, *, as_image=False, as_voice=False, voice_ms=0):
            captured.update(as_image=as_image, as_voice=as_voice, path=path)
            return wx.SendResult(success=True, message_id="m1")
        monkeypatch.setattr(ch, "_send_file", fake_send_file)

        res = await ch.send_voice("user@im", str(src))
        assert res.success
        assert captured["as_voice"] is False  # fell back to file attachment
        assert captured["path"] == str(src)

    @pytest.mark.asyncio
    async def test_voice_over_60s_falls_back_to_file(self, tmp_path, monkeypatch):
        ch = _make_weixin(tmp_path)
        src = tmp_path / "long.mp3"
        src.write_bytes(b"ID3 fake mp3")

        async def fake_encode(_src):
            return (str(tmp_path / "x.silk"), 61_000)
        monkeypatch.setattr(wx, "encode_to_silk", fake_encode)
        (tmp_path / "x.silk").write_bytes(b"\x02#!SILK_V3")

        captured = {}
        async def fake_send_file(chat_id, path, *, as_image=False, as_voice=False, voice_ms=0):
            captured.update(as_voice=as_voice)
            return wx.SendResult(success=True)
        monkeypatch.setattr(ch, "_send_file", fake_send_file)

        res = await ch.send_voice("user@im", str(src))
        assert res.success
        assert captured["as_voice"] is False  # >60s → file

    @pytest.mark.asyncio
    async def test_voice_happy_path_sends_voice(self, tmp_path, monkeypatch):
        ch = _make_weixin(tmp_path)
        src = tmp_path / "ok.mp3"
        src.write_bytes(b"ID3 fake mp3")
        silk = tmp_path / "ok.silk"
        silk.write_bytes(b"\x02#!SILK_V3")

        async def fake_encode(_src):
            return (str(silk), 3000)
        monkeypatch.setattr(wx, "encode_to_silk", fake_encode)

        captured = {}
        async def fake_send_file(chat_id, path, *, as_image=False, as_voice=False, voice_ms=0):
            captured.update(as_voice=as_voice, voice_ms=voice_ms, path=path)
            return wx.SendResult(success=True, message_id="m2")
        monkeypatch.setattr(ch, "_send_file", fake_send_file)

        res = await ch.send_voice("user@im", str(src))
        assert res.success
        assert captured["as_voice"] is True
        assert captured["voice_ms"] == 3000
        assert captured["path"] == str(silk)

    @pytest.mark.asyncio
    async def test_voice_send_failure_reports_failure(self, tmp_path, monkeypatch):
        ch = _make_weixin(tmp_path)
        src = tmp_path / "f.mp3"
        src.write_bytes(b"ID3 fake mp3")
        silk = tmp_path / "f.silk"
        silk.write_bytes(b"\x02#!SILK_V3")

        async def fake_encode(_src):
            return (str(silk), 2000)
        monkeypatch.setattr(wx, "encode_to_silk", fake_encode)

        # Both voice and fallback file fail → honest failure, no false-positive.
        async def fail_send_file(chat_id, path, *, as_image=False, as_voice=False, voice_ms=0):
            return wx.SendResult(success=False, error="ret:-1")
        monkeypatch.setattr(ch, "_send_file", fail_send_file)

        res = await ch.send_voice("user@im", str(src))
        assert res.success is False


