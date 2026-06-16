"""Tests for 8 previously untested channels:
CronChannel, DingTalkChannel, EmailChannel, FeishuChannel,
WebhookChannel, WeComChannel, WhatsAppChannel, qqbot_media helpers.
"""
from __future__ import annotations

import asyncio
import hashlib
import hmac
import json
import time
from unittest.mock import AsyncMock, MagicMock, patch

import pytest



# ── Helpers ──────────────────────────────────────────────────────────────────


def _mock_bus():
    bus = MagicMock()
    bus.publish_inbound = AsyncMock(return_value=True)
    bus.subscribe_outbound = MagicMock()
    return bus


# ══════════════════════════════════════════════════════════════════════════════
# 1. CronChannel
# ══════════════════════════════════════════════════════════════════════════════


class TestCronChannel:
    def _make(self):
        from echo_agent.channels.cron import CronChannel

        config = MagicMock()
        config.allow_from = []
        bus = _mock_bus()
        ch = CronChannel(config, bus)
        return ch, bus

    @pytest.mark.asyncio
    async def test_init_and_start(self):
        ch, bus = self._make()
        await ch.start()
        assert ch._running is True
        assert ch.name == "cron"
        bus.subscribe_outbound.assert_called_once_with("cron", ch.send)

    @pytest.mark.asyncio
    async def test_send_returns_success(self):
        from echo_agent.bus.events import OutboundEvent

        ch, _ = self._make()
        await ch.start()
        event = OutboundEvent.text_reply(channel="cron", chat_id="cron:job1", text="hello")
        result = await ch.send(event)
        assert result.success is True

    @pytest.mark.asyncio
    async def test_inject_success(self):
        ch, bus = self._make()
        await ch.start()
        await ch.inject("daily_report", "Run report")
        bus.publish_inbound.assert_called_once()
        event = bus.publish_inbound.call_args[0][0]
        assert event.chat_id == "cron:daily_report"
        assert event.content[0].text == "Run report"

    @pytest.mark.asyncio
    async def test_inject_rejected_raises(self):
        ch, bus = self._make()
        bus.publish_inbound = AsyncMock(return_value=False)
        await ch.start()
        with pytest.raises(RuntimeError, match="rejected"):
            await ch.inject("failing_job", "test")

    @pytest.mark.asyncio
    async def test_inject_with_deliver_channel(self):
        ch, bus = self._make()
        await ch.start()
        await ch.inject("j1", "msg", deliver_channel="telegram")
        event = bus.publish_inbound.call_args[0][0]
        assert event.metadata["deliver_channel"] == "telegram"

    @pytest.mark.asyncio
    async def test_stop(self):
        ch, _ = self._make()
        await ch.start()
        await ch.stop()
        assert ch._running is False


# ══════════════════════════════════════════════════════════════════════════════
# 2. DingTalkChannel
# ══════════════════════════════════════════════════════════════════════════════


class TestDingTalkChannel:
    def _make(self):
        from echo_agent.channels.dingtalk import DingTalkChannel

        config = MagicMock()
        config.app_key = "key123"
        config.app_secret = "secret456"
        config.robot_code = "robot_abc"
        config.allow_from = []
        bus = _mock_bus()
        ch = DingTalkChannel(config, bus)
        return ch, bus

    def test_init_attributes(self):
        ch, _ = self._make()
        assert ch.name == "dingtalk"
        assert ch._app_key == "key123"
        assert ch._robot_code == "robot_abc"

    @pytest.mark.asyncio
    async def test_send_1to1(self):
        from echo_agent.bus.events import OutboundEvent

        ch, _ = self._make()
        ch._session = MagicMock()
        ch._access_token = "tok"
        ch._token_expires = time.time() + 9999

        mock_resp = AsyncMock()
        mock_resp.status = 200
        mock_resp.__aenter__ = AsyncMock(return_value=mock_resp)
        mock_resp.__aexit__ = AsyncMock(return_value=False)
        ch._session.post = MagicMock(return_value=mock_resp)

        event = OutboundEvent.text_reply(channel="dingtalk", chat_id="user1", text="hi")
        event.metadata = {}
        result = await ch.send(event)
        assert result.success is True
        call_args = ch._session.post.call_args
        assert "oToMessages" in call_args[0][0]

    @pytest.mark.asyncio
    async def test_send_group(self):
        from echo_agent.bus.events import OutboundEvent

        ch, _ = self._make()
        ch._session = MagicMock()
        ch._access_token = "tok"
        ch._token_expires = time.time() + 9999

        mock_resp = AsyncMock()
        mock_resp.status = 200
        mock_resp.__aenter__ = AsyncMock(return_value=mock_resp)
        mock_resp.__aexit__ = AsyncMock(return_value=False)
        ch._session.post = MagicMock(return_value=mock_resp)

        event = OutboundEvent.text_reply(channel="dingtalk", chat_id="conv123", text="hi group")
        event.metadata = {"conversation_type": "2"}
        result = await ch.send(event)
        assert result.success is True
        call_args = ch._session.post.call_args
        assert "groupMessages" in call_args[0][0]

    @pytest.mark.asyncio
    async def test_send_no_text(self):
        from echo_agent.bus.events import OutboundEvent

        ch, _ = self._make()
        ch._session = MagicMock()
        event = OutboundEvent.text_reply(channel="dingtalk", chat_id="u1", text="")
        result = await ch.send(event)
        assert result.success is False

    @pytest.mark.asyncio
    async def test_send_http_error(self):
        from echo_agent.bus.events import OutboundEvent

        ch, _ = self._make()
        ch._session = MagicMock()
        ch._access_token = "tok"
        ch._token_expires = time.time() + 9999

        mock_resp = AsyncMock()
        mock_resp.status = 403
        mock_resp.text = AsyncMock(return_value="forbidden")
        mock_resp.__aenter__ = AsyncMock(return_value=mock_resp)
        mock_resp.__aexit__ = AsyncMock(return_value=False)
        ch._session.post = MagicMock(return_value=mock_resp)

        event = OutboundEvent.text_reply(channel="dingtalk", chat_id="u1", text="test")
        event.metadata = {}
        result = await ch.send(event)
        assert result.success is False
        assert "forbidden" in result.error

    @pytest.mark.asyncio
    async def test_on_message_text(self):

        ch, bus = self._make()
        ch._running = True
        ch.bus = bus
        ch._session = MagicMock()
        ch._access_token = "tok"
        ch._token_expires = time.time() + 9999

        # Mock _handle_message
        ch._handle_message = AsyncMock()
        data = {
            "conversationId": "conv1",
            "senderStaffId": "staff1",
            "conversationType": "1",
            "text": {"content": "hello dingtalk"},
            "msgtype": "text",
        }
        await ch._on_message(data)
        ch._handle_message.assert_called_once()
        call_kwargs = ch._handle_message.call_args[1]
        assert call_kwargs["text"] == "hello dingtalk"
        assert call_kwargs["sender_id"] == "staff1"


# ══════════════════════════════════════════════════════════════════════════════
# 3. EmailChannel
# ══════════════════════════════════════════════════════════════════════════════


class TestEmailChannel:
    def _make(self):
        from echo_agent.channels.email import EmailChannel

        config = MagicMock()
        config.imap_host = "imap.test.com"
        config.imap_port = 993
        config.smtp_host = "smtp.test.com"
        config.smtp_port = 465
        config.username = "bot@test.com"
        config.password = "pass123"
        config.use_ssl = True
        config.poll_interval_seconds = 30
        config.allow_from = []
        bus = _mock_bus()
        ch = EmailChannel(config, bus)
        return ch, bus

    def test_init(self):
        ch, _ = self._make()
        assert ch.name == "email"

    def test_parse_address_with_angle_brackets(self):
        from echo_agent.channels.email import EmailChannel
        assert EmailChannel._parse_address("User <user@example.com>") == "user@example.com"

    def test_parse_address_plain(self):
        from echo_agent.channels.email import EmailChannel
        assert EmailChannel._parse_address("plain@example.com") == "plain@example.com"

    def test_decode_header_ascii(self):
        from echo_agent.channels.email import EmailChannel
        assert EmailChannel._decode_header("Hello World") == "Hello World"

    def test_decode_header_encoded(self):
        from echo_agent.channels.email import EmailChannel
        encoded = "=?utf-8?B?5rWL6K+V?="  # "测试" in base64
        result = EmailChannel._decode_header(encoded)
        assert "测试" in result

    def test_extract_body_plain(self):
        from echo_agent.channels.email import EmailChannel
        import email.mime.text
        msg = email.mime.text.MIMEText("Hello body", "plain", "utf-8")
        result = EmailChannel._extract_body(msg)
        assert result == "Hello body"

    def test_extract_body_multipart(self):
        from echo_agent.channels.email import EmailChannel
        from email.mime.multipart import MIMEMultipart
        from email.mime.text import MIMEText

        msg = MIMEMultipart()
        msg.attach(MIMEText("Plain text part", "plain", "utf-8"))
        msg.attach(MIMEText("<p>HTML part</p>", "html", "utf-8"))
        result = EmailChannel._extract_body(msg)
        assert result == "Plain text part"

    @pytest.mark.asyncio
    async def test_send_smtp_success(self):
        from echo_agent.bus.events import OutboundEvent

        ch, _ = self._make()
        ch._subject_map = {"user@test.com": "Test Subject"}

        with patch("echo_agent.channels.email.smtplib") as mock_smtp_mod:
            mock_smtp = MagicMock()
            mock_smtp_mod.SMTP_SSL.return_value.__enter__ = MagicMock(return_value=mock_smtp)
            mock_smtp_mod.SMTP_SSL.return_value.__exit__ = MagicMock(return_value=False)

            event = OutboundEvent.text_reply(channel="email", chat_id="user@test.com", text="Reply")
            result = await ch.send(event)
            assert result.success is True

    @pytest.mark.asyncio
    async def test_send_no_text(self):
        from echo_agent.bus.events import OutboundEvent

        ch, _ = self._make()
        event = OutboundEvent.text_reply(channel="email", chat_id="user@test.com", text="")
        result = await ch.send(event)
        assert result.success is False


# ══════════════════════════════════════════════════════════════════════════════
# 4. FeishuChannel
# ══════════════════════════════════════════════════════════════════════════════


class TestFeishuChannel:
    def _make(self, encryption_key=""):
        from echo_agent.channels.feishu import FeishuChannel

        config = MagicMock()
        config.app_id = "app123"
        config.app_secret = "secret456"
        config.verification_token = "vtoken"
        config.encryption_key = encryption_key
        config.webhook_path = "/feishu"
        config.host = "0.0.0.0"
        config.port = 8083
        config.allow_from = []
        bus = _mock_bus()
        ch = FeishuChannel(config, bus)
        return ch, bus

    def test_init_attributes(self):
        ch, _ = self._make()
        assert ch.name == "feishu"
        assert ch._app_id == "app123"
        assert ch._verification_token == "vtoken"

    @pytest.mark.asyncio
    async def test_send_success(self):
        from echo_agent.bus.events import OutboundEvent

        ch, _ = self._make()
        ch._session = MagicMock()
        ch._tenant_token = "tenant_tok"
        ch._token_expires = time.time() + 9999

        mock_resp = AsyncMock()
        mock_resp.json = AsyncMock(return_value={"code": 0})
        mock_resp.__aenter__ = AsyncMock(return_value=mock_resp)
        mock_resp.__aexit__ = AsyncMock(return_value=False)
        ch._session.post = MagicMock(return_value=mock_resp)

        event = OutboundEvent.text_reply(channel="feishu", chat_id="chat1", text="hello")
        event.metadata = {"receive_id_type": "chat_id"}
        event.reply_to_id = None
        result = await ch.send(event)
        assert result.success is True

    @pytest.mark.asyncio
    async def test_send_reply_mode(self):
        from echo_agent.bus.events import OutboundEvent

        ch, _ = self._make()
        ch._session = MagicMock()
        ch._tenant_token = "tenant_tok"
        ch._token_expires = time.time() + 9999

        mock_resp = AsyncMock()
        mock_resp.json = AsyncMock(return_value={"code": 0})
        mock_resp.__aenter__ = AsyncMock(return_value=mock_resp)
        mock_resp.__aexit__ = AsyncMock(return_value=False)
        ch._session.post = MagicMock(return_value=mock_resp)

        event = OutboundEvent.text_reply(channel="feishu", chat_id="chat1", text="reply")
        event.metadata = {"receive_id_type": "chat_id"}
        event.reply_to_id = "msg_123"
        result = await ch.send(event)
        assert result.success is True
        call_url = ch._session.post.call_args[0][0]
        assert "msg_123/reply" in call_url

    @pytest.mark.asyncio
    async def test_send_no_session(self):
        from echo_agent.bus.events import OutboundEvent

        ch, _ = self._make()
        ch._session = None
        event = OutboundEvent.text_reply(channel="feishu", chat_id="c1", text="x")
        result = await ch.send(event)
        assert result.success is False

    def test_decrypt_no_cryptography(self):
        ch, _ = self._make(encryption_key="mykey")
        with patch.dict("sys.modules", {"cryptography": None, "cryptography.hazmat.primitives.ciphers": None}):
            # _decrypt should handle ImportError gracefully
            result = ch._decrypt("invalid_base64_data")
            assert result is None

    @pytest.mark.asyncio
    async def test_webhook_url_verification(self):

        ch, _ = self._make()
        ch._running = True

        # Simulate url_verification request
        request = MagicMock()
        request.read = AsyncMock(return_value=json.dumps({
            "type": "url_verification",
            "challenge": "test_challenge_123",
        }).encode())

        resp = await ch._webhook(request)
        body = json.loads(resp.body)
        assert body["challenge"] == "test_challenge_123"

    @pytest.mark.asyncio
    async def test_webhook_token_mismatch(self):
        ch, _ = self._make()
        ch._running = True

        request = MagicMock()
        request.read = AsyncMock(return_value=json.dumps({
            "schema": "2.0",
            "header": {"token": "wrong_token", "event_id": "e1", "event_type": "im.message.receive_v1"},
            "event": {},
        }).encode())

        resp = await ch._webhook(request)
        assert resp.status == 403


# ══════════════════════════════════════════════════════════════════════════════
# 5. WebhookChannel
# ══════════════════════════════════════════════════════════════════════════════


class TestWebhookChannel:
    def _make(self, secret=""):
        from echo_agent.channels.webhook import WebhookChannel

        config = MagicMock()
        config.host = "0.0.0.0"
        config.port = 8080
        config.path = "/webhook"
        config.secret = secret
        config.allow_from = []
        bus = _mock_bus()
        ch = WebhookChannel(config, bus)
        return ch, bus

    def test_init(self):
        ch, _ = self._make()
        assert ch.name == "webhook"

    def test_verify_signature_no_secret(self):
        ch, _ = self._make(secret="")
        assert ch._verify_signature(b"anything", "") is True

    def test_verify_signature_valid(self):
        ch, _ = self._make(secret="mysecret")
        body = b'{"text": "hello"}'
        expected = hmac.new(b"mysecret", body, hashlib.sha256).hexdigest()
        assert ch._verify_signature(body, expected) is True

    def test_verify_signature_invalid(self):
        ch, _ = self._make(secret="mysecret")
        body = b'{"text": "hello"}'
        assert ch._verify_signature(body, "bad_signature") is False

    @pytest.mark.asyncio
    async def test_send_sync_mode(self):
        """In sync mode, send resolves a pending future."""
        from echo_agent.bus.events import OutboundEvent

        ch, _ = self._make()
        loop = asyncio.get_running_loop()
        future = loop.create_future()
        ch._pending_responses["evt-1"] = future

        event = OutboundEvent.text_reply(channel="webhook", chat_id="wh", text="response")
        event.reply_to_id = "evt-1"
        result = await ch.send(event)
        assert result.success is True
        assert future.result() == "response"

    @pytest.mark.asyncio
    async def test_send_async_mode(self):
        """Without a pending future, send still succeeds."""
        from echo_agent.bus.events import OutboundEvent

        ch, _ = self._make()
        event = OutboundEvent.text_reply(channel="webhook", chat_id="wh", text="response")
        event.reply_to_id = "no-such-id"
        result = await ch.send(event)
        assert result.success is True

    @pytest.mark.asyncio
    async def test_send_resolves_future_by_inbound_event_id(self):
        """P0-5: outbound 用 metadata._inbound_event_id 关联 pending future。"""
        from echo_agent.bus.events import OutboundEvent

        ch, _ = self._make()
        loop = asyncio.get_running_loop()
        future = loop.create_future()
        ch._pending_responses["evt-42"] = future

        event = OutboundEvent.text_reply(channel="webhook", chat_id="wh", text="done")
        event.metadata = {"_inbound_event_id": "evt-42"}
        result = await ch.send(event)
        assert result.success is True
        assert future.result() == "done"


# ══════════════════════════════════════════════════════════════════════════════
# 6. WeComChannel
# ══════════════════════════════════════════════════════════════════════════════


class TestWeComChannel:
    def _make(self):
        from echo_agent.channels.wecom import WeComChannel

        config = MagicMock()
        config.corp_id = "corp123"
        config.agent_id = "1000001"
        config.secret = "sec"
        config.token = "mytoken"
        config.encoding_aes_key = ""
        config.webhook_path = "/wecom"
        config.host = "0.0.0.0"
        config.port = 8084
        config.allow_from = []
        bus = _mock_bus()
        ch = WeComChannel(config, bus)
        return ch, bus

    def test_init(self):
        ch, _ = self._make()
        assert ch.name == "wecom"
        assert ch._corp_id == "corp123"

    def test_check_signature_valid(self):
        ch, _ = self._make()
        timestamp = "1234567890"
        nonce = "nonce123"
        items = sorted(["mytoken", timestamp, nonce])
        expected = hashlib.sha1("".join(items).encode()).hexdigest()
        assert ch._check_signature(expected, timestamp, nonce) is True

    def test_check_signature_invalid(self):
        ch, _ = self._make()
        assert ch._check_signature("invalid", "123", "nonce") is False

    @pytest.mark.asyncio
    async def test_send_success(self):
        from echo_agent.bus.events import OutboundEvent

        ch, _ = self._make()
        ch._session = MagicMock()
        ch._access_token = "access_tok"
        ch._token_expires = time.time() + 9999

        mock_resp = AsyncMock()
        mock_resp.json = AsyncMock(return_value={"errcode": 0, "errmsg": "ok"})
        mock_resp.__aenter__ = AsyncMock(return_value=mock_resp)
        mock_resp.__aexit__ = AsyncMock(return_value=False)
        ch._session.post = MagicMock(return_value=mock_resp)

        event = OutboundEvent.text_reply(channel="wecom", chat_id="user1", text="hello wecom")
        result = await ch.send(event)
        assert result.success is True

    @pytest.mark.asyncio
    async def test_send_api_error(self):
        from echo_agent.bus.events import OutboundEvent

        ch, _ = self._make()
        ch._session = MagicMock()
        ch._access_token = "tok"
        ch._token_expires = time.time() + 9999

        mock_resp = AsyncMock()
        mock_resp.json = AsyncMock(return_value={"errcode": 40001, "errmsg": "invalid token"})
        mock_resp.__aenter__ = AsyncMock(return_value=mock_resp)
        mock_resp.__aexit__ = AsyncMock(return_value=False)
        ch._session.post = MagicMock(return_value=mock_resp)

        event = OutboundEvent.text_reply(channel="wecom", chat_id="user1", text="test")
        result = await ch.send(event)
        assert result.success is False
        assert "40001" in result.error

    @pytest.mark.asyncio
    async def test_send_no_text(self):
        from echo_agent.bus.events import OutboundEvent

        ch, _ = self._make()
        ch._session = MagicMock()
        event = OutboundEvent.text_reply(channel="wecom", chat_id="u1", text="")
        result = await ch.send(event)
        assert result.success is False


# ══════════════════════════════════════════════════════════════════════════════
# 7. WhatsAppChannel
# ══════════════════════════════════════════════════════════════════════════════


class TestWhatsAppChannel:
    def _make(self):
        from echo_agent.channels.whatsapp import WhatsAppChannel

        config = MagicMock()
        config.verify_token = "my_verify"
        config.access_token = "fb_token"
        config.phone_number_id = "phone123"
        config.webhook_path = "/whatsapp"
        config.host = "0.0.0.0"
        config.port = 8081
        config.allow_from = []
        bus = _mock_bus()
        ch = WhatsAppChannel(config, bus)
        return ch, bus

    def test_init(self):
        ch, _ = self._make()
        assert ch.name == "whatsapp"
        assert ch._phone_id == "phone123"

    @pytest.mark.asyncio
    async def test_send_success(self):
        from echo_agent.bus.events import OutboundEvent

        ch, _ = self._make()
        ch._session = MagicMock()

        mock_resp = AsyncMock()
        mock_resp.status = 200
        mock_resp.__aenter__ = AsyncMock(return_value=mock_resp)
        mock_resp.__aexit__ = AsyncMock(return_value=False)
        ch._session.post = MagicMock(return_value=mock_resp)

        event = OutboundEvent.text_reply(channel="whatsapp", chat_id="+1234567890", text="hi")
        result = await ch.send(event)
        assert result.success is True
        call_url = ch._session.post.call_args[0][0]
        assert "phone123/messages" in call_url

    @pytest.mark.asyncio
    async def test_send_no_text(self):
        from echo_agent.bus.events import OutboundEvent

        ch, _ = self._make()
        ch._session = MagicMock()
        event = OutboundEvent.text_reply(channel="whatsapp", chat_id="+1", text="")
        result = await ch.send(event)
        assert result.success is False

    @pytest.mark.asyncio
    async def test_send_http_error(self):
        from echo_agent.bus.events import OutboundEvent

        ch, _ = self._make()
        ch._session = MagicMock()

        mock_resp = AsyncMock()
        mock_resp.status = 401
        mock_resp.text = AsyncMock(return_value="Unauthorized")
        mock_resp.__aenter__ = AsyncMock(return_value=mock_resp)
        mock_resp.__aexit__ = AsyncMock(return_value=False)
        ch._session.post = MagicMock(return_value=mock_resp)

        event = OutboundEvent.text_reply(channel="whatsapp", chat_id="+1", text="test")
        result = await ch.send(event)
        assert result.success is False
        assert "Unauthorized" in result.error

    @pytest.mark.asyncio
    async def test_verify_success(self):
        ch, _ = self._make()
        request = MagicMock()
        request.query = {
            "hub.mode": "subscribe",
            "hub.verify_token": "my_verify",
            "hub.challenge": "challenge_abc",
        }
        resp = await ch._verify(request)
        assert resp.text == "challenge_abc"

    @pytest.mark.asyncio
    async def test_verify_failure(self):
        ch, _ = self._make()
        request = MagicMock()
        request.query = {
            "hub.mode": "subscribe",
            "hub.verify_token": "wrong_token",
            "hub.challenge": "challenge_abc",
        }
        resp = await ch._verify(request)
        assert resp.status == 403


# ══════════════════════════════════════════════════════════════════════════════
# 8. qqbot_media helpers
# ══════════════════════════════════════════════════════════════════════════════


class TestQQBotMedia:
    def test_get_clean_extension_normal(self):
        from echo_agent.channels.qqbot_media import get_clean_extension
        assert get_clean_extension("/path/to/file.png") == ".png"

    def test_get_clean_extension_with_query(self):
        from echo_agent.channels.qqbot_media import get_clean_extension
        assert get_clean_extension("https://cdn.com/img.jpg?token=abc") == ".jpg"

    def test_get_clean_extension_no_ext(self):
        from echo_agent.channels.qqbot_media import get_clean_extension
        assert get_clean_extension("noextension") == ""

    def test_is_image_file_by_ext(self):
        from echo_agent.channels.qqbot_media import is_image_file
        assert is_image_file("test.png") is True
        assert is_image_file("test.mp3") is False

    def test_is_image_file_by_mime(self):
        from echo_agent.channels.qqbot_media import is_image_file
        assert is_image_file("noext", mime_type="image/jpeg") is True

    def test_detect_media_kind(self):
        from echo_agent.channels.qqbot_media import detect_media_kind
        assert detect_media_kind("song.mp3") == "voice"
        assert detect_media_kind("clip.mp4") == "video"
        assert detect_media_kind("photo.png") == "image"
        assert detect_media_kind("doc.pdf") == "file"

    def test_detect_media_kind_by_mime(self):
        from echo_agent.channels.qqbot_media import detect_media_kind
        assert detect_media_kind("noext", mime_type="audio/mpeg") == "voice"
        assert detect_media_kind("noext", mime_type="video/mp4") == "video"

    def test_normalize_media_tags_self_closing(self):
        from echo_agent.channels.qqbot_media import normalize_media_tags
        text = '<qqimg src="test.png" />'
        result = normalize_media_tags(text)
        assert "<qqimg>test.png</qqimg>" in result

    def test_normalize_media_tags_alias(self):
        from echo_agent.channels.qqbot_media import normalize_media_tags
        text = '<image src="photo.jpg" />'
        result = normalize_media_tags(text)
        assert "<qqimg>" in result

    def test_parse_send_queue_text_only(self):
        from echo_agent.channels.qqbot_media import parse_send_queue
        items = parse_send_queue("just text")
        assert len(items) == 1
        assert items[0].kind == "text"
        assert items[0].content == "just text"

    def test_parse_send_queue_with_media(self):
        from echo_agent.channels.qqbot_media import parse_send_queue
        text = "Hello <qqimg>test.png</qqimg> world"
        items = parse_send_queue(text)
        assert any(i.kind == "image" and "test.png" in i.content for i in items)
        assert any(i.kind == "text" for i in items)

    def test_parse_send_queue_multiple_media(self):
        from echo_agent.channels.qqbot_media import parse_send_queue
        text = "<qqimg>a.png</qqimg><qqvoice>b.mp3</qqvoice>"
        items = parse_send_queue(text)
        kinds = [i.kind for i in items]
        assert "image" in kinds
        assert "voice" in kinds

    def test_parse_send_queue_empty(self):
        from echo_agent.channels.qqbot_media import parse_send_queue
        items = parse_send_queue("")
        assert items == []
