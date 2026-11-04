"""Channel ingress and lifecycle regressions from the Runtime review."""

from __future__ import annotations

import asyncio
import json
from unittest.mock import AsyncMock

import pytest

from echo_agent.bus.queue import MessageBus
from echo_agent.channels.manager import ChannelManager
from echo_agent.channels.feishu import FeishuChannel
from echo_agent.channels.matrix import MatrixChannel
from echo_agent.channels.telegram import TelegramChannel
from echo_agent.channels.webhook import WebhookChannel
from echo_agent.channels.wecom import WeComChannel
from echo_agent.channels.whatsapp import WhatsAppChannel
from echo_agent.config.schema import (
    ChannelsConfig, FeishuChannelConfig, MatrixChannelConfig,
    TelegramChannelConfig, WebhookChannelConfig, WeComChannelConfig, WhatsAppChannelConfig,
)


class _Request:
    headers: dict = {}
    query: dict = {}

    def __init__(self, body):
        self.body = body

    async def read(self):
        return json.dumps(self.body).encode()

    async def text(self):
        return self.body


@pytest.mark.asyncio
async def test_wecom_rejects_unaccepted_message_for_platform_retry():
    bus = MessageBus()
    bus.publish_inbound = AsyncMock(return_value=False)
    bus.publish_outbound = AsyncMock()
    channel = WeComChannel(WeComChannelConfig(token="test-secret"), bus)
    channel._check_signature = lambda *_: True
    request = _Request("<xml><MsgType>text</MsgType><FromUserName>alice</FromUserName>"
                       "<MsgId>123</MsgId><Content>test marker</Content></xml>")
    response = await channel._webhook(request)
    assert response.status == 503
    bus.publish_inbound.assert_awaited_once()


@pytest.mark.asyncio
@pytest.mark.parametrize("body", [
    {"text": ["invalid"]},
    {"text": {"nested": "invalid"}},
    {"text": "valid", "metadata": ["invalid"]},
    {"text": "valid", "wait": "false"},
])
async def test_webhook_rejects_malformed_input_before_bus_admission(body):
    bus = MessageBus()
    bus.publish_inbound = AsyncMock(return_value=True)
    channel = WebhookChannel(WebhookChannelConfig(), bus)
    response = await asyncio.wait_for(channel._handle_webhook(_Request(body)), timeout=0.2)
    assert response.status == 400
    bus.publish_inbound.assert_not_awaited()


@pytest.mark.asyncio
async def test_cancelled_channel_start_cleans_partial_resources(monkeypatch):
    config = ChannelsConfig()
    config.cron.enabled = True
    bus = MessageBus()
    manager = ChannelManager(config, bus)
    opened = asyncio.Event()

    class PartialChannel:
        is_running = False
        send = AsyncMock()
        stop = AsyncMock()

        async def start(self):
            bus.subscribe_outbound("cron", self.send)
            opened.set()
            await asyncio.Event().wait()

    channel = PartialChannel()
    monkeypatch.setattr(manager, "_build_channel", lambda _: channel)
    task = asyncio.create_task(manager.start_channel("cron"))
    await opened.wait()
    task.cancel()
    with pytest.raises(asyncio.CancelledError):
        await task
    channel.stop.assert_awaited_once()
    assert not bus._outbound_handlers.get("cron")


@pytest.mark.asyncio
async def test_repeated_start_all_reuses_cleanup_task():
    manager = ChannelManager(ChannelsConfig(cli={"enabled": False}), MessageBus())
    await manager.start_all()
    first = manager._cleanup_task
    try:
        await manager.start_all()
        assert manager._cleanup_task is first
    finally:
        await manager.stop_all()
        if first is not None and not first.done():
            first.cancel()
            await asyncio.gather(first, return_exceptions=True)


@pytest.mark.asyncio
async def test_telegram_unreadable_attachment_is_not_silently_acknowledged(tmp_path):
    bus = MessageBus()
    bus.publish_inbound = AsyncMock(return_value=True)
    channel = TelegramChannel(TelegramChannelConfig(data_dir=str(tmp_path)), bus)
    channel._download_telegram_file = AsyncMock(return_value=None)
    channel._api = AsyncMock()
    accepted = await channel._process_update({"message": {
        "message_id": 99, "chat": {"id": 12, "type": "private"},
        "from": {"id": 8}, "document": {"file_id": "expired", "file_name": "notes.pdf"},
    }})
    assert accepted is True
    bus.publish_inbound.assert_awaited_once()
    event = bus.publish_inbound.call_args.args[0]
    assert "未读取" in event.text
    assert not event.media_items


@pytest.mark.asyncio
@pytest.mark.parametrize("name", ["feishu", "matrix", "whatsapp"])
async def test_failed_media_download_preserves_explicit_unreadable_notice(name, tmp_path, monkeypatch):
    monkeypatch.setattr("echo_agent.runtime_paths.echo_home", lambda: tmp_path)
    bus = MessageBus()
    bus.publish_inbound = AsyncMock(return_value=True)
    if name == "feishu":
        channel = FeishuChannel(FeishuChannelConfig(), bus)
        channel._download_feishu_image = AsyncMock(return_value=None)
        accepted = await channel._on_message({
            "sender": {"sender_id": {"open_id": "alice"}, "sender_type": "user"},
            "message": {"chat_id": "chat", "message_id": "msg", "chat_type": "p2p",
                        "message_type": "image", "content": '{"image_key":"expired"}'},
        })
    elif name == "matrix":
        channel = MatrixChannel(MatrixChannelConfig(user_id="@bot:local"), bus)
        channel._download_matrix_media = AsyncMock(return_value=None)
        accepted = await channel._on_event("!room:local", {
            "type": "m.room.message", "sender": "@alice:local", "event_id": "$event",
            "content": {"msgtype": "m.image", "url": "mxc://local/expired", "body": "image.jpg"},
        })
    else:
        channel = WhatsAppChannel(WhatsAppChannelConfig(), bus)
        channel._download_whatsapp_media = AsyncMock(return_value=None)
        accepted = await channel._process_message({
            "id": "msg", "from": "alice", "type": "image", "image": {"id": "expired"},
        }, {}, [])
    assert accepted is True
    bus.publish_inbound.assert_awaited_once()
    event = bus.publish_inbound.call_args.args[0]
    assert "未读取" in event.text
    assert not event.media_items


@pytest.mark.asyncio
async def test_timed_out_channel_start_does_not_block_other_channels(monkeypatch):
    import echo_agent.channels.manager as module

    monkeypatch.setattr(module, "_CHANNEL_START_TIMEOUT", 0.02, raising=False)
    config = ChannelsConfig(cli={"enabled": False}, cron={"enabled": True}, webhook={"enabled": True})
    manager = ChannelManager(config, MessageBus())

    class SlowChannel:
        is_running = False
        send = AsyncMock()
        stop = AsyncMock()

        async def start(self):
            await asyncio.Event().wait()

    real_build = manager._build_channel
    slow = SlowChannel()
    monkeypatch.setattr(manager, "_build_channel", lambda name: slow if name == "webhook" else real_build(name))
    try:
        await asyncio.wait_for(manager.start_all(), timeout=0.5)
        assert manager.active_channels == ["cron"]
        slow.stop.assert_awaited_once()
    finally:
        await manager.stop_all()


@pytest.mark.asyncio
@pytest.mark.parametrize("name", ["discord", "qqbot"])
async def test_gateway_rejection_keeps_replay_sequence_and_releases_dedup(name):
    from types import SimpleNamespace
    from echo_agent.channels.discord import DiscordChannel
    from echo_agent.channels.qqbot import QQBotChannel
    from echo_agent.config.schema import DiscordChannelConfig, QQBotChannelConfig

    bus = MessageBus()
    bus.publish_inbound = AsyncMock(return_value=False)
    bus.publish_outbound = AsyncMock()
    if name == "discord":
        channel = DiscordChannel(DiscordChannelConfig(group_policy="open"), bus)
        message_type = "MESSAGE_CREATE"
        payload = {"id": "msg", "channel_id": "room", "content": "marker", "author": {"id": "alice"}}
    else:
        channel = QQBotChannel(QQBotChannelConfig(), bus)
        message_type = "C2C_MESSAGE_CREATE"
        payload = {"id": "msg", "content": "marker", "author": {"user_openid": "alice"}}
    channel._seq = 10
    channel._ws = SimpleNamespace(closed=False, close=AsyncMock())
    frame = {"op": 0, "s": 11, "t": message_type, "d": payload}
    await channel._handle_ws_message(frame)
    assert channel._seq == 10
    channel._ws.close.assert_awaited_once()
    # Resume and replay the same message after local backpressure clears.
    bus.publish_inbound.return_value = True
    await channel._handle_ws_message(frame)
    assert channel._seq == 11
    assert bus.publish_inbound.await_count == 2
    events = [call.args[0] for call in bus.publish_inbound.call_args_list]
    assert events[0].event_id == events[1].event_id


@pytest.mark.asyncio
@pytest.mark.parametrize("name", ["slack", "matrix", "whatsapp"])
async def test_downloaded_document_enters_actual_context_with_correct_format(name, tmp_path, monkeypatch):
    from echo_agent.agent.context import ContextBuilder
    from echo_agent.channels.slack import SlackChannel
    from echo_agent.config.schema import SlackChannelConfig

    monkeypatch.setattr("echo_agent.runtime_paths.echo_home", lambda: tmp_path)
    bus = MessageBus()
    bus.publish_inbound = AsyncMock(return_value=True)
    if name == "slack":
        channel = SlackChannel(SlackChannelConfig(), bus)
        event = {"type": "message", "channel": "chat", "channel_type": "im", "user": "alice", "ts": "1",
                 "files": [{"url_private": "https://files.example/opaque", "name": "标记.txt", "mimetype": "text/plain"}]}
        receive = channel._on_event(event)
    elif name == "matrix":
        channel = MatrixChannel(MatrixChannelConfig(user_id="@bot:local"), bus)
        event = {"type": "m.room.message", "sender": "@alice:local", "event_id": "$file",
                 "content": {"msgtype": "m.file", "url": "mxc://local/opaque", "body": "标记.txt",
                             "info": {"mimetype": "text/plain"}}}
        receive = channel._on_event("!room:local", event)
    else:
        channel = WhatsAppChannel(WhatsAppChannelConfig(), bus)
        event = {"id": "msg", "from": "alice", "type": "document",
                 "document": {"id": "opaque", "filename": "标记.txt", "mime_type": "text/plain"}}
        receive = channel._process_message(event, {}, [])
    channel._media_cache_root = tmp_path / "cache"
    channel._session = object()
    if name == "whatsapp":
        from unittest.mock import MagicMock
        response = MagicMock()
        response.__aenter__ = AsyncMock(return_value=response)
        response.__aexit__ = AsyncMock(return_value=False)
        response.status = 200
        response.json = AsyncMock(return_value={"url": "https://files.example/opaque"})
        channel._session = MagicMock()
        channel._session.get.return_value = response
    channel._fetch_with_limit = AsyncMock(return_value="document context marker 文档内容".encode())
    await receive
    inbound = bus.publish_inbound.call_args.args[0]
    assert inbound.media_items[0].metadata["name"] == "标记.txt"
    assert inbound.media_items[0].mime_type == "text/plain"
    builder = ContextBuilder(workspace=tmp_path)
    resolved = await builder.resolve_inbound_media(inbound.media_items, channel=name)
    messages = builder.build_messages([], "请读取附件", media=resolved)
    assert "document context marker 文档内容" in json.dumps(messages, ensure_ascii=False)
