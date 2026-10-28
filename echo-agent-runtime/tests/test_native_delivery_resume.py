"""Real native adapters preserve text and resume from SQLite-confirmed receipts."""
import html
import json
from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest

from echo_agent.agent.turn_run_store import TurnRunStore
from echo_agent.bus.events import OutboundEvent
from echo_agent.bus.queue import MessageBus
from echo_agent.channels.base import DeliveryRejected, SendResult
from echo_agent.config import schema
from echo_agent.storage.sqlite import SQLiteBackend


@pytest.mark.asyncio
@pytest.mark.parametrize("name", ["telegram", "discord", "slack", "feishu", "weixin", "wecom", "matrix"])
async def test_native_adapter_retry_preserves_text_and_skips_confirmed_parts(tmp_path, monkeypatch, name):
    from echo_agent.channels.telegram import TelegramChannel
    from echo_agent.channels.discord import DiscordChannel
    from echo_agent.channels.slack import SlackChannel
    from echo_agent.channels.feishu import FeishuChannel
    from echo_agent.channels.wecom import WeComChannel
    from echo_agent.channels.matrix import MatrixChannel
    from echo_agent.channels import weixin
    storage = SQLiteBackend(tmp_path / "native.db")
    await storage.initialize()
    bus = MessageBus()
    bus.set_turn_run_store(TurnRunStore(storage))
    classes = {"telegram": TelegramChannel, "discord": DiscordChannel, "slack": SlackChannel,
               "feishu": FeishuChannel, "weixin": weixin.WeixinChannel, "wecom": WeComChannel, "matrix": MatrixChannel}
    config_cls = schema.WeComChannelConfig if name == "wecom" else getattr(schema, f"{name.capitalize()}ChannelConfig")
    # Telegram/Weixin directories are isolated from the operator's live account.
    config = config_cls(data_dir=str(tmp_path)) if name in {"telegram", "weixin", "matrix"} else config_cls()
    channel = classes[name](config, bus)
    channel._session = object()
    sent = []
    def record(text):
        sent.append(text)
        return len(sent) != 2
    if name == "telegram":
        async def api(method, **kwargs):
            if not record(kwargs["json"]["text"]):
                raise DeliveryRejected("rate limited")
            return {"message_id": len(sent)}
        channel._api = api
    elif name == "discord":
        async def post(url, payload):
            return SendResult(success=record(payload["content"]), message_id=str(len(sent)), error="rejected")
        channel._post_message = post
    elif name == "slack":
        async def api(method, **kwargs):
            return {"ok": record(kwargs["json_body"]["text"]), "ts": str(len(sent)), "error": "rate_limited"}
        channel._api = api
    elif name in {"feishu", "wecom", "matrix"}:
        class Response:
            async def __aenter__(self):
                return self
            async def __aexit__(self, *args):
                pass
            async def json(self):
                return self.body
        def post(url, **kwargs):
            payload = kwargs["json"]
            text = json.loads(payload["content"])["text"] if name == "feishu" else payload["text"]["content"] if name == "wecom" else payload["body"]
            accepted = record(text)
            response = Response()
            response.status = 200 if accepted else 429
            response.body = ({"code": 0 if accepted else 999, "data": {"message_id": str(len(sent))}}
                             if name == "feishu" else {"errcode": 0, "msgid": str(len(sent))}
                             if name == "wecom" else {"event_id": str(len(sent))})
            return response
        channel._session = SimpleNamespace(post=post, put=post)
        if name == "feishu":
            channel._ensure_tenant_token = AsyncMock()
        elif name == "wecom":
            channel._ensure_token = AsyncMock()
    else:
        channel._send_session, channel._token = object(), "fixture"
        async def send(session, **kwargs):
            return {"ret": 0 if record(kwargs["text"]) else 1}
        monkeypatch.setattr(weixin, "_send_message", send)
    bus.subscribe_outbound(name, channel.send)
    text = "  中文<&>🙂\n" * 1600 + "  end\n"
    if name == "matrix":
        text *= 2
    event = OutboundEvent.text_reply(name, "chat", text, event_id="native-reply")
    try:
        assert not (await bus.publish_outbound(event)).ok
        result = await bus.publish_outbound(event)
        assert result.ok
        assert sent[1] == sent[2]
        # Failed part is attempted twice; the first acknowledged part only once.
        complete = "".join([sent[0], *sent[2:]])
        assert (html.unescape(complete) if name == "telegram" else complete) == text
        assert len(result.detail["message_ids"]) == len(sent) - 1
        assert await bus._turn_run_store.list_outbound() == []
    finally:
        await storage.close()


@pytest.mark.asyncio
@pytest.mark.parametrize("name", ["telegram", "discord", "slack", "feishu", "weixin", "wecom", "matrix"])
async def test_missing_native_acknowledgement_stays_unknown_and_is_not_replayed(tmp_path, monkeypatch, name):
    from unittest.mock import MagicMock
    from echo_agent.channels.telegram import TelegramChannel
    from echo_agent.channels.discord import DiscordChannel
    from echo_agent.channels.slack import SlackChannel
    from echo_agent.channels.feishu import FeishuChannel
    from echo_agent.channels.wecom import WeComChannel
    from echo_agent.channels.matrix import MatrixChannel
    from echo_agent.channels import weixin
    storage = SQLiteBackend(tmp_path / "missing.db")
    await storage.initialize()
    bus = MessageBus()
    bus.set_turn_run_store(TurnRunStore(storage))
    classes = {"telegram": TelegramChannel, "discord": DiscordChannel, "slack": SlackChannel,
               "feishu": FeishuChannel, "weixin": weixin.WeixinChannel, "wecom": WeComChannel, "matrix": MatrixChannel}
    config_cls = schema.WeComChannelConfig if name == "wecom" else getattr(schema, f"{name.capitalize()}ChannelConfig")
    config = config_cls(data_dir=str(tmp_path)) if name in {"telegram", "weixin", "matrix"} else config_cls()
    channel = classes[name](config, bus)
    if name in {"telegram", "slack"}:
        sender = AsyncMock(return_value={} if name == "telegram" else {"ok": True})
        channel._api = sender
    elif name == "weixin":
        channel._send_session, channel._token = object(), "fixture"
        sender = AsyncMock(return_value={"unexpected": "proxy response"})
        monkeypatch.setattr(weixin, "_send_message", sender)
    else:
        class Response:
            status = 200
            async def __aenter__(self):
                return self
            async def __aexit__(self, *args):
                pass
            async def json(self):
                return {"code": 0} if name == "feishu" else {}
        sender = MagicMock(return_value=Response())
        channel._session = SimpleNamespace(post=sender, put=sender)
        if name == "feishu":
            channel._ensure_tenant_token = AsyncMock()
        elif name == "wecom":
            channel._ensure_token = AsyncMock()
    bus.subscribe_outbound(name, channel.send)
    event = OutboundEvent.text_reply(name, "chat", "完整内容")
    try:
        first = await bus.publish_outbound(event)
        assert not first.ok and first.detail["delivery_unknown"]
        second = await bus.publish_outbound(event)
        assert not second.ok and second.detail["delivery_unknown"]
        assert sender.call_count == 1
        assert (await bus._turn_run_store.list_outbound())[0]["status"] == "unknown"
    finally:
        await storage.close()


@pytest.mark.asyncio
async def test_matrix_attachment_failure_is_truthful_and_transactions_are_unique_and_stable(tmp_path):
    from echo_agent.channels.matrix import MatrixChannel
    from echo_agent.bus.events import ContentBlock, ContentType
    storage = SQLiteBackend(tmp_path / "matrix-media.db")
    await storage.initialize()
    bus = MessageBus()
    bus.set_turn_run_store(TurnRunStore(storage))
    channel = MatrixChannel(schema.MatrixChannelConfig(data_dir=str(tmp_path)), bus)
    calls = []
    class Response:
        async def __aenter__(self):
            return self
        async def __aexit__(self, *args):
            pass
        async def json(self):
            return {"event_id": f"receipt-{len(calls)}"}
    def put(url, **kwargs):
        calls.append((url, kwargs["json"]))
        response = Response()
        response.status = 404 if len(calls) == 2 else 200
        return response
    channel._session = SimpleNamespace(put=put)
    bus.subscribe_outbound("matrix", channel.send)
    event = OutboundEvent(channel="matrix", chat_id="room", content=[ContentBlock(text="说明"),
        ContentBlock(type=ContentType.IMAGE, url="mxc://example/one"),
        ContentBlock(type=ContentType.FILE, url="mxc://example/two")])
    try:
        assert not (await bus.publish_outbound(event)).ok
        assert len(calls) == 2 and calls[0][0] != calls[1][0]
        assert (await bus.publish_outbound(event)).ok
        assert len(calls) == 4 and calls[1][0] == calls[2][0]
        assert calls[3][1]["body"] == "说明"
    finally:
        await storage.close()
