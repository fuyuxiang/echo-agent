"""QQBot send() downgrades GFM markdown before dispatching to the channel.

Verifies the normalization layer is wired into the send path so tables,
headings and HR never reach QQ as raw source, and that inline markers are
kept only when we actually send QQ native markdown (msg_type=2).
"""

from __future__ import annotations

from unittest.mock import AsyncMock, MagicMock

import pytest

from echo_agent.bus.events import ContentBlock, ContentType, OutboundEvent
from echo_agent.bus.queue import MessageBus
from echo_agent.channels.qqbot import QQBotChannel


def _make_channel(*, markdown: bool) -> QQBotChannel:
    cfg = MagicMock()
    cfg.app_id = "x"
    cfg.app_secret = "y"
    cfg.sandbox = False
    cfg.markdown_support = markdown
    cfg.media_enabled = False
    cfg.media_parse_tags = False
    cfg.media_max_file_size_mb = 10
    cfg.media_upload_cache_size = 4
    ch = QQBotChannel(cfg, MessageBus())
    # Bypass the network entirely; capture the text that would be sent.
    ch._session = MagicMock()
    ch._ensure_token = AsyncMock()
    ch.should_deliver = lambda event: True  # type: ignore[assignment]
    return ch


def _event(text: str, msg_type: str = "group") -> OutboundEvent:
    return OutboundEvent(
        channel="qqbot",
        chat_id="c1",
        content=[ContentBlock(type=ContentType.TEXT, text=text)],
        metadata={"msg_type": msg_type},
    )


async def _capture_send(ch: QQBotChannel, event: OutboundEvent) -> list[str]:
    sent: list[str] = []

    async def fake_send_chunk(chat_id, text, msg_type, reply_to):
        sent.append(text)
        return True

    ch._send_chunk = fake_send_chunk  # type: ignore[assignment]
    await ch.send(event)
    return sent


@pytest.mark.asyncio
async def test_table_downgraded_in_plain_mode() -> None:
    ch = _make_channel(markdown=False)
    text = "| 项目 | 状态 |\n|------|------|\n| 部署 | 完成 |"
    sent = await _capture_send(ch, _event(text))
    joined = "\n".join(sent)
    assert "项目: 部署" in joined
    assert "状态: 完成" in joined
    assert "|" not in joined


@pytest.mark.asyncio
async def test_inline_stripped_in_plain_mode() -> None:
    ch = _make_channel(markdown=False)
    sent = await _capture_send(ch, _event("这是 **重点**"))
    assert sent == ["这是 重点"]


@pytest.mark.asyncio
async def test_inline_kept_in_markdown_mode() -> None:
    ch = _make_channel(markdown=True)
    sent = await _capture_send(ch, _event("这是 **重点**"))
    assert sent == ["这是 **重点**"]


@pytest.mark.asyncio
async def test_heading_and_hr_downgraded_both_modes() -> None:
    for markdown in (False, True):
        ch = _make_channel(markdown=markdown)
        sent = await _capture_send(ch, _event("## 标题\n---\n正文"))
        joined = "\n".join(sent)
        assert "标题" in joined
        assert "正文" in joined
        assert "#" not in joined
        assert "---" not in joined


@pytest.mark.asyncio
async def test_channel_type_strips_inline_even_when_markdown_on() -> None:
    # Guild "channel" messages send plain content, so inline markers must be
    # stripped regardless of markdown_support.
    ch = _make_channel(markdown=True)
    sent = await _capture_send(ch, _event("这是 **重点**", msg_type="channel"))
    assert sent == ["这是 重点"]
