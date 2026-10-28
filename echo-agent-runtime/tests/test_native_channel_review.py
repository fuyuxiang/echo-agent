"""Native ingress authentication, durable identities, and delivery integrity."""

import hashlib
import asyncio
import json
from unittest.mock import AsyncMock, MagicMock

import pytest

from echo_agent.bus.events import OutboundEvent
from echo_agent.bus.queue import MessageBus
from echo_agent.channels.feishu import FeishuChannel
from echo_agent.channels.whatsapp import WhatsAppChannel
from echo_agent.config.schema import FeishuChannelConfig, WhatsAppChannelConfig


@pytest.mark.asyncio
@pytest.mark.parametrize("kind", ["feishu", "whatsapp"])
async def test_missing_native_webhook_secret_fails_closed(kind):
    bus = MessageBus()
    channel = (FeishuChannel(FeishuChannelConfig(), bus) if kind == "feishu"
               else WhatsAppChannel(WhatsAppChannelConfig(), bus))
    request = MagicMock()
    request.headers = {}
    payload = ({"schema": "2.0", "header": {"event_id": "attack", "event_type": "im.message.receive_v1"},
                "event": {"sender": {"sender_type": "user", "sender_id": {"open_id": "owner"}},
                          "message": {"chat_id": "chat", "chat_type": "p2p", "message_type": "text",
                                      "message_id": "m1", "content": json.dumps({"text": "run task"})}}}
               if kind == "feishu" else
               {"entry": [{"changes": [{"value": {"messages": [
                   {"id": "attack", "from": "owner", "type": "text", "text": {"body": "run task"}},
               ]}}]}]})
    request.read = AsyncMock(return_value=json.dumps(payload).encode())
    result = await channel._webhook(request)
    assert result.status == 403
    assert bus.pending_inbound == 0


@pytest.mark.asyncio
async def test_feishu_configured_encrypt_key_requires_body_signature():
    channel = FeishuChannel(FeishuChannelConfig(encryption_key="encrypt", verification_token="verify"), MessageBus())
    request = MagicMock(headers={})
    request.read = AsyncMock(return_value=json.dumps({"schema": "2.0", "header": {
        "token": "verify", "event_id": "event", "event_type": "ignored",
    }}).encode())
    assert (await channel._webhook(request)).status == 403


class _Response:
    def __init__(self, status, payload):
        self.status, self.payload = status, payload

    async def __aenter__(self):
        return self

    async def __aexit__(self, *args):
        return False

    async def json(self):
        return self.payload

    async def text(self):
        return json.dumps(self.payload)


@pytest.mark.asyncio
async def test_whatsapp_missing_receipt_and_server_failure_are_unknown():
    channel = WhatsAppChannel(WhatsAppChannelConfig(), MessageBus())
    channel._session = MagicMock()
    channel._session.post.return_value = _Response(200, {})
    event = OutboundEvent.text_reply("whatsapp", "peer", "hello")
    result = await channel.send(event)
    assert not result.success and result.delivery_unknown
    channel._session.post.return_value = _Response(503, {"error": "unavailable"})
    result = await channel.send(OutboundEvent.text_reply("whatsapp", "peer", "hello"))
    assert not result.success and result.delivery_unknown


@pytest.mark.asyncio
async def test_weixin_voice_unknown_outcome_never_falls_back_to_a_second_send(tmp_path, monkeypatch):
    from echo_agent.channels import weixin
    from echo_agent.channels.base import SendResult
    from echo_agent.config.schema import WeixinChannelConfig

    source = tmp_path / "voice.wav"
    source.write_bytes(b"voice")
    channel = weixin.WeixinChannel(WeixinChannelConfig(data_dir=str(tmp_path)), MessageBus())
    channel._send_session, channel._token = MagicMock(), "test"
    monkeypatch.setattr(weixin, "encode_to_silk", AsyncMock(return_value=(None, 10)))
    channel._send_file = AsyncMock(return_value=SendResult(success=False, delivery_unknown=True, error="timeout"))
    result = await channel.send_voice("peer", str(source))
    assert not result.success and result.delivery_unknown
    assert channel._send_file.await_count == 1


@pytest.mark.asyncio
async def test_qq_media_does_not_blindly_retry_an_unknown_send():
    from echo_agent.channels.qqbot_media import send_media_message
    session = MagicMock()
    session.post.side_effect = [_Response(503, {"error": "unknown"}), _Response(200, {"id": "duplicate"})]
    with pytest.raises(RuntimeError):
        await send_media_message(session, "https://qq.invalid", {}, "c2c", "peer", "file", 1)
    assert session.post.call_count == 1


@pytest.mark.asyncio
async def test_feishu_valid_signed_body_is_accepted_without_a_plain_token():
    channel = FeishuChannel(FeishuChannelConfig(encryption_key="encrypt"), MessageBus())
    body = json.dumps({"schema": "2.0", "header": {"event_type": "ignored"}}).encode()
    signature = hashlib.sha256(b"123nonceencrypt" + body).hexdigest()
    request = MagicMock(headers={"X-Lark-Request-Timestamp": "123", "X-Lark-Request-Nonce": "nonce",
                                 "X-Lark-Signature": signature})
    request.read = AsyncMock(return_value=body)
    assert (await channel._webhook(request)).status == 200


@pytest.mark.asyncio
@pytest.mark.parametrize("kind", ["discord", "qqbot"])
async def test_channel_stop_joins_cancelled_background_work(kind):
    from echo_agent.channels.discord import DiscordChannel
    from echo_agent.channels.qqbot import QQBotChannel
    from echo_agent.config.schema import DiscordChannelConfig, QQBotChannelConfig
    cleaned = asyncio.Event()

    async def worker():
        try:
            await asyncio.Event().wait()
        except asyncio.CancelledError:
            await asyncio.sleep(0.01)
            cleaned.set()

    channel = (DiscordChannel(DiscordChannelConfig(), MessageBus()) if kind == "discord"
               else QQBotChannel(QQBotChannelConfig(), MessageBus()))
    task = asyncio.create_task(worker())
    await asyncio.sleep(0)
    channel._heartbeat_task = task
    try:
        await channel.stop()
        assert cleaned.is_set()
        assert task.done()
    finally:
        await task


@pytest.mark.asyncio
@pytest.mark.parametrize("kind", ["telegram", "discord", "slack", "feishu", "matrix", "whatsapp", "weixin", "qqbot"])
async def test_native_redelivery_keeps_durable_turn_identity(kind, tmp_path):
    from echo_agent.channels.telegram import TelegramChannel
    from echo_agent.channels.discord import DiscordChannel
    from echo_agent.channels.slack import SlackChannel
    from echo_agent.channels.matrix import MatrixChannel
    from echo_agent.channels.weixin import WeixinChannel
    from echo_agent.channels.qqbot import QQBotChannel
    from echo_agent.config.schema import (TelegramChannelConfig, DiscordChannelConfig, SlackChannelConfig,
                                         MatrixChannelConfig, WeixinChannelConfig, QQBotChannelConfig)
    from echo_agent.storage.sqlite import SQLiteBackend
    from echo_agent.agent.turn_run_store import TurnRunStore

    bus = MessageBus()
    if kind == "telegram":
        channel = TelegramChannel(TelegramChannelConfig(data_dir=str(tmp_path)), bus)
        channel._api = AsyncMock(return_value={})
        async def deliver():
            await channel._process_update({"message": {"message_id": 1, "chat": {"id": "peer", "type": "private"},
                                                     "from": {"id": "user"}, "text": "work"}})
    elif kind == "discord":
        channel = DiscordChannel(DiscordChannelConfig(), bus)
        async def deliver():
            await channel._on_message({"id": "1", "author": {"id": "user"}, "channel_id": "peer", "content": "work"})
    elif kind == "slack":
        channel = SlackChannel(SlackChannelConfig(), bus)
        async def deliver():
            await channel._on_event({"type": "message", "user": "user", "channel": "peer", "channel_type": "im",
                                     "ts": "1", "text": "work"})
    elif kind == "feishu":
        channel = FeishuChannel(FeishuChannelConfig(), bus)
        async def deliver():
            await channel._on_message({"sender": {"sender_id": {"open_id": "user"}}, "message": {
                "message_id": "1", "chat_id": "peer", "chat_type": "p2p", "message_type": "text",
                "content": json.dumps({"text": "work"})}})
    elif kind == "matrix":
        channel = MatrixChannel(MatrixChannelConfig(), bus)
        async def deliver():
            await channel._on_event("peer", {"event_id": "1", "type": "m.room.message", "sender": "user",
                                             "content": {"msgtype": "m.text", "body": "work"}})
    elif kind == "whatsapp":
        channel = WhatsAppChannel(WhatsAppChannelConfig(), bus)
        async def deliver():
            channel._seen_message_ids.clear()  # simulate adapter/process restart
            await channel._process_message({"id": "1", "from": "peer", "type": "text", "text": {"body": "work"}}, {}, [])
    elif kind == "weixin":
        channel = WeixinChannel(WeixinChannelConfig(data_dir=str(tmp_path), account_id="bot"), bus)
        async def deliver():
            channel._dedup._seen.clear()
            await channel._process_message({"message_id": "1", "from_user_id": "peer", "item_list": [
                {"type": 1, "text_item": {"text": "work"}},
            ]})
    else:
        channel = QQBotChannel(QQBotChannelConfig(), bus)
        async def deliver():
            channel._seen_messages.clear()
            await channel._on_c2c_message({"id": "1", "author": {"user_openid": "peer"}, "content": "work"})

    await deliver()
    await deliver()
    first, second = await bus._inbound_queue.get(), await bus._inbound_queue.get()
    assert first.event_id == second.event_id
    storage = SQLiteBackend(tmp_path / "turns.sqlite")
    await storage.initialize()
    try:
        ledger = TurnRunStore(storage)
        assert await ledger.mark_running(first.event_id, first.session_key, context_key=first.session_key, trace_id="first")
        assert not await ledger.mark_running(second.event_id, second.session_key, context_key=second.session_key, trace_id="duplicate")
    finally:
        await storage.close()


@pytest.mark.asyncio
async def test_telegram_rejected_admission_does_not_advance_offset(tmp_path):
    from echo_agent.channels.telegram import TelegramChannel
    from echo_agent.config.schema import TelegramChannelConfig
    bus = MessageBus()
    bus._accepting = False
    channel = TelegramChannel(TelegramChannelConfig(data_dir=str(tmp_path)), bus)
    channel._running, channel._bot_id = True, "bot"
    polls = 0

    async def api(method, **kwargs):
        nonlocal polls
        if method != "getUpdates":
            return {}
        polls += 1
        if polls > 1:
            channel._running = False
            return []
        return [{"update_id": 1, "message": {"message_id": 1, "chat": {"id": "peer", "type": "private"},
                                              "from": {"id": "user"}, "text": "work"}}]

    channel._api = api
    await channel._poll_loop()
    assert channel._offset == 0


@pytest.mark.asyncio
@pytest.mark.parametrize("accepted", [True, False])
async def test_matrix_resumes_first_batch_and_only_commits_accepted_cursor(accepted, tmp_path, monkeypatch):
    from echo_agent.channels.matrix import MatrixChannel
    from echo_agent.config.schema import MatrixChannelConfig
    monkeypatch.setattr(MatrixChannel, "_resolve_state_path", lambda self: tmp_path / "matrix.json")
    bus = MessageBus()
    bus._accepting = accepted
    channel = MatrixChannel(MatrixChannelConfig(), bus)
    channel._since, channel._running = "old", True
    polls = 0

    async def sync(**kwargs):
        nonlocal polls
        polls += 1
        if polls > 1:
            channel._running = False
            return None
        return {"next_batch": "new", "rooms": {"join": {"peer": {"timeline": {"events": [
            {"event_id": "1", "type": "m.room.message", "sender": "user",
             "content": {"msgtype": "m.text", "body": "work"}},
        ]}}}}}

    channel._do_sync = sync
    await asyncio.wait_for(channel._sync_loop(), timeout=5)
    assert channel._since == ("new" if accepted else "old")
    assert bus.pending_inbound == (1 if accepted else 0)


@pytest.mark.asyncio
async def test_whatsapp_partial_send_recovers_only_after_unknown_resolution(tmp_path):
    from echo_agent.agent.turn_run_store import TurnRunStore
    from echo_agent.storage.sqlite import SQLiteBackend
    storage = SQLiteBackend(tmp_path / "delivery.sqlite")
    await storage.initialize()
    try:
        bus = MessageBus()
        store = TurnRunStore(storage)
        bus.set_turn_run_store(store)
        session = MagicMock()
        session.post.side_effect = [_Response(200, {"messages": [{"id": "first"}]}),
                                    _Response(503, {"error": "unknown"}),
                                    _Response(200, {"messages": [{"id": "second"}]}),
                                    _Response(200, {"messages": [{"id": "third"}]})]
        event = OutboundEvent.text_reply("whatsapp", "peer", "x" * 8001)
        first = WhatsAppChannel(WhatsAppChannelConfig(), bus)
        first._session = session
        result = await first.send(event)
        assert not result.success and result.delivery_unknown
        assert result.message_ids == ["first"]
        restarted = WhatsAppChannel(WhatsAppChannelConfig(), bus)
        restarted._session = session
        assert (await restarted.send(event)).delivery_unknown
        assert session.post.call_count == 2
        await store.resolve_outbound(event.event_id, confirmed_not_delivered=True)
        result = await restarted.send(event)
        assert result.success and result.message_ids == ["first", "second", "third"]
        assert session.post.call_count == 4
    finally:
        await storage.close()


@pytest.mark.asyncio
async def test_feishu_busy_admission_is_retryable_without_poisoned_dedup():
    bus = MessageBus()
    bus._accepting = False
    channel = FeishuChannel(FeishuChannelConfig(verification_token="verify"), bus)
    payload = {"schema": "2.0", "header": {"token": "verify", "event_id": "one", "event_type": "im.message.receive_v1"},
               "event": {"sender": {"sender_id": {"open_id": "user"}}, "message": {
                   "message_id": "one", "message_type": "text", "chat_type": "p2p", "chat_id": "peer",
                   "content": json.dumps({"text": "work"})}}}
    request = MagicMock(headers={})
    request.read = AsyncMock(return_value=json.dumps(payload).encode())
    assert (await channel._webhook(request)).status == 503
    assert "one" not in channel._seen_ids
    bus._accepting = True
    assert (await channel._webhook(request)).status == 200
    assert bus.pending_inbound == 1


@pytest.mark.asyncio
async def test_whatsapp_local_attachment_uploads_then_sends_media_id(tmp_path):
    from echo_agent.bus.events import ContentBlock, ContentType
    path = tmp_path / "report.pdf"
    path.write_bytes(b"%PDF-test")
    channel = WhatsAppChannel(WhatsAppChannelConfig(phone_number_id="phone"), MessageBus())
    channel._session = MagicMock()
    channel._session.post.side_effect = [_Response(200, {"id": "uploaded-media"}),
                                        _Response(200, {"messages": [{"id": "sent-file"}]})]
    event = OutboundEvent(channel="whatsapp", chat_id="peer", content=[ContentBlock(type=ContentType.FILE, url=str(path))])
    result = await channel.send(event)
    assert result.success and result.message_id == "sent-file"
    assert channel.supports_files
    upload, send = channel._session.post.call_args_list
    assert upload.args[0].endswith("/phone/media")
    assert send.kwargs["json"]["document"] == {"id": "uploaded-media"}


@pytest.mark.asyncio
async def test_weixin_mixed_delivery_retains_partial_receipts_and_unknown_fence(tmp_path):
    from echo_agent.bus.events import ContentBlock, ContentType
    from echo_agent.channels import weixin
    from echo_agent.channels.base import SendResult
    from echo_agent.config.schema import WeixinChannelConfig
    from echo_agent.storage.sqlite import SQLiteBackend
    from echo_agent.agent.turn_run_store import TurnRunStore
    storage = SQLiteBackend(tmp_path / "mixed.sqlite")
    await storage.initialize()
    try:
        bus = MessageBus()
        bus.set_turn_run_store(TurnRunStore(storage))
        channel = weixin.WeixinChannel(WeixinChannelConfig(data_dir=str(tmp_path)), bus)
        channel._send_session, channel._token = MagicMock(), "test"
        channel._send_text_chunk = AsyncMock(return_value=SendResult(success=True, message_id="caption"))
        channel.send_file = AsyncMock(return_value=SendResult(success=False, error="unknown", delivery_unknown=True))
        event = OutboundEvent(channel="weixin", chat_id="peer", content=[
            ContentBlock(type=ContentType.TEXT, text="caption"), ContentBlock(type=ContentType.FILE, url="/tmp/report.pdf"),
        ])
        first = await channel.send(event)
        assert first.delivery_unknown and first.message_ids == ["caption"]
        second = await channel.send(event)
        assert second.delivery_unknown and second.message_ids == ["caption"]
        channel._send_text_chunk.assert_awaited_once()
        channel.send_file.assert_awaited_once()
    finally:
        await storage.close()
