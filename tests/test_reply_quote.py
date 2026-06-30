"""引用回复(quote / reply-to)跨通道能力测试。

覆盖三层:
  1. 理解层:build_user_message_with_reply 把被引用原文注入用户消息(三分支)
  2. 入站层:Telegram/Discord 从平台字段解析被引用消息填进 InboundEvent
  3. 出站层:被引用消息已删时去掉锚点降级重发(Telegram/Discord)
"""

from __future__ import annotations

import copy
from unittest.mock import AsyncMock, MagicMock

import pytest

from echo_agent.agent.pipeline.context_stage import build_user_message_with_reply
from echo_agent.bus.events import InboundEvent
from echo_agent.bus.queue import MessageBus
from echo_agent.config.schema import DiscordChannelConfig


def _evt(text: str, **kwargs) -> InboundEvent:
    return InboundEvent.text_message(
        channel="test", sender_id="u1", chat_id="c1", text=text, **kwargs
    )


# ── 1. 理解层:注入逻辑 ─────────────────────────────────────────────────────────

class TestBuildUserMessageWithReply:
    def test_no_reply_returns_text_unchanged(self):
        evt = _evt("hello")
        assert build_user_message_with_reply(evt) == "hello"

    def test_quote_other_user_prepends_sender_and_text(self):
        evt = _evt("改改这个", reply_to_text="原始方案", reply_to_sender="Alice")
        out = build_user_message_with_reply(evt)
        assert out == '[引用 Alice: "原始方案"]\n\n改改这个'

    def test_quote_without_sender_uses_generic_prefix(self):
        evt = _evt("继续", reply_to_text="前文")
        assert build_user_message_with_reply(evt) == '[引用: "前文"]\n\n继续'

    def test_quote_own_message_uses_own_prefix(self):
        evt = _evt("第二点展开", reply_to_text="机器人的回答", reply_to_is_own=True)
        out = build_user_message_with_reply(evt)
        assert out == '[回复你刚才的消息: "机器人的回答"]\n\n第二点展开'

    def test_blank_reply_text_not_injected(self):
        evt = _evt("hi", reply_to_text="   ", reply_to_sender="Bob")
        assert build_user_message_with_reply(evt) == "hi"

    def test_long_reply_text_truncated(self):
        evt = _evt("ok", reply_to_text="x" * 1000, reply_to_sender="Bob")
        out = build_user_message_with_reply(evt)
        assert out.count("x") == 500  # 截断到 _REPLY_SNIPPET_MAX

    def test_quote_with_empty_user_text_returns_prefix_only(self):
        evt = _evt("", reply_to_text="原文", reply_to_sender="Alice")
        assert build_user_message_with_reply(evt) == '[引用 Alice: "原文"]'

    def test_does_not_mutate_event_text(self):
        evt = _evt("改改", reply_to_text="原文", reply_to_sender="Alice")
        build_user_message_with_reply(evt)
        assert evt.text == "改改"  # 检索/压缩仍用原始问题


# ── 2. 入站层:平台解析被引用消息 ───────────────────────────────────────────────

class _Captured:
    """捕获 _handle_message 收到的引用字段。"""
    def __init__(self):
        self.kwargs = None


def _make_telegram():
    from echo_agent.channels.telegram import TelegramChannel
    from echo_agent.config.schema import TelegramChannelConfig
    ch = TelegramChannel(TelegramChannelConfig(enabled=True, token="t"), MessageBus())
    ch._bot_id = "999"
    return ch


@pytest.mark.asyncio
async def test_telegram_inbound_parses_quoted_message():
    ch = _make_telegram()
    cap = _Captured()

    async def fake_handle(**kwargs):
        cap.kwargs = kwargs

    ch._handle_message = fake_handle
    ch._api = AsyncMock(return_value=None)
    msg = {
        "message_id": 50, "chat": {"id": 1, "type": "private"},
        "from": {"id": 7}, "text": "改改",
        "reply_to_message": {"text": "原始方案", "from": {"id": 7, "first_name": "Alice"}},
    }
    await ch._process_update({"message": msg})
    assert cap.kwargs["reply_to_text"] == "原始方案"
    assert cap.kwargs["reply_to_sender"] == "Alice"
    assert cap.kwargs["reply_to_is_own"] is False
    assert cap.kwargs["reply_to_id"] == "50"  # 锚点仍是本条消息


@pytest.mark.asyncio
async def test_telegram_inbound_detects_reply_to_own_message():
    ch = _make_telegram()
    cap = _Captured()

    async def fake_handle(**kwargs):
        cap.kwargs = kwargs

    ch._handle_message = fake_handle
    ch._api = AsyncMock(return_value=None)
    msg = {
        "message_id": 51, "chat": {"id": 1, "type": "private"},
        "from": {"id": 7}, "text": "展开第二点",
        "reply_to_message": {"text": "机器人回答", "from": {"id": 999, "first_name": "Bot"}},
    }
    await ch._process_update({"message": msg})
    assert cap.kwargs["reply_to_is_own"] is True


def _make_discord():
    from echo_agent.channels.discord import DiscordChannel
    ch = DiscordChannel(DiscordChannelConfig(enabled=True, token="t"), MessageBus())
    ch._bot_id = "999"
    return ch


@pytest.mark.asyncio
async def test_discord_inbound_parses_referenced_message():
    ch = _make_discord()
    cap = _Captured()

    async def fake_handle(**kwargs):
        cap.kwargs = kwargs

    ch._handle_message = fake_handle
    d = {
        "id": "60", "channel_id": "ch1", "author": {"id": "7"}, "content": "改改",
        "referenced_message": {"content": "原始方案", "author": {"id": "7", "username": "Alice"}},
    }
    await ch._on_message(d)
    assert cap.kwargs["reply_to_text"] == "原始方案"
    assert cap.kwargs["reply_to_sender"] == "Alice"
    assert cap.kwargs["reply_to_is_own"] is False


@pytest.mark.asyncio
async def test_discord_inbound_detects_reply_to_own():
    ch = _make_discord()
    cap = _Captured()

    async def fake_handle(**kwargs):
        cap.kwargs = kwargs

    ch._handle_message = fake_handle
    d = {
        "id": "61", "channel_id": "ch1", "author": {"id": "7"}, "content": "展开",
        "referenced_message": {"content": "机器人回答", "author": {"id": "999", "username": "Bot"}},
    }
    await ch._on_message(d)
    assert cap.kwargs["reply_to_is_own"] is True


# ── 3. 出站层:被引用消息已删时降级重发 ─────────────────────────────────────────

class _SeqResp:
    """按调用顺序返回预设响应的 aiohttp ClientResponse 替身。"""
    def __init__(self, status: int, data: dict):
        self.status = status
        self._data = data

    async def json(self):
        return self._data

    async def text(self):
        return str(self._data)

    async def __aenter__(self):
        return self

    async def __aexit__(self, *a):
        return False


@pytest.mark.asyncio
async def test_telegram_send_drops_anchor_when_reply_target_gone():
    from echo_agent.bus.events import OutboundEvent
    ch = _make_telegram()
    ch._session = MagicMock()
    calls = []
    # 第一次带锚点 → 报 replied not found；第二次去锚点 → 成功
    responses = [
        _SeqResp(400, {"ok": False, "description": "Bad Request: message to be replied not found"}),
        _SeqResp(200, {"ok": True, "result": {"message_id": 123}}),
    ]

    def post(url, **kwargs):
        calls.append(copy.deepcopy(kwargs.get("json", {})))
        return responses[len(calls) - 1]

    ch._session.post = post
    result = await ch.send(OutboundEvent.text_reply("telegram", "c1", "答复", reply_to_id="50"))
    assert result.success
    assert len(calls) == 2
    assert "reply_to_message_id" in calls[0]      # 首次带锚点
    assert "reply_to_message_id" not in calls[1]  # 重发去掉锚点


@pytest.mark.asyncio
async def test_telegram_send_keeps_anchor_on_success():
    from echo_agent.bus.events import OutboundEvent
    ch = _make_telegram()
    ch._session = MagicMock()
    calls = []

    def post(url, **kwargs):
        calls.append(copy.deepcopy(kwargs.get("json", {})))
        return _SeqResp(200, {"ok": True, "result": {"message_id": 1}})

    ch._session.post = post
    result = await ch.send(OutboundEvent.text_reply("telegram", "c1", "答复", reply_to_id="50"))
    assert result.success
    assert len(calls) == 1  # 成功路径不重试


@pytest.mark.asyncio
async def test_discord_send_drops_anchor_when_reference_invalid():
    from echo_agent.bus.events import OutboundEvent
    ch = _make_discord()
    ch._session = MagicMock()
    calls = []
    responses = [
        _SeqResp(400, {"code": 50035, "message": "Invalid Form Body"}),
        _SeqResp(200, {"id": "789"}),
    ]

    def post(url, **kwargs):
        calls.append(copy.deepcopy(kwargs.get("json", {})))
        return responses[len(calls) - 1]

    ch._session.post = post
    result = await ch.send(OutboundEvent.text_reply("discord", "c1", "答复", reply_to_id="60"))
    assert result.success
    assert len(calls) == 2
    assert "message_reference" in calls[0]
    assert "message_reference" not in calls[1]



