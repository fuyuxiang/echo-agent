"""CLI channel streaming — chunks print incrementally, the final full-text
message must not duplicate what was already printed."""

from __future__ import annotations

import pytest

from echo_agent.bus.events import OutboundEvent
from echo_agent.bus.queue import MessageBus
from echo_agent.channels.cli import CLIChannel
from echo_agent.config.schema import CLIChannelConfig


def _make_channel() -> CLIChannel:
    return CLIChannel(CLIChannelConfig(), MessageBus())


def _stream_event(text: str, *, final: bool, event_id: str = "ev1", full_text: bool = False) -> OutboundEvent:
    event = OutboundEvent.text_reply(channel="cli", chat_id="cli", text=text)
    event.message_kind = "final" if final else "streaming"
    event.is_final = final
    event.metadata = {"_token_stream": True, "_inbound_event_id": event_id}
    if full_text:
        event.metadata["_stream_full_text"] = True
    return event


@pytest.mark.asyncio
async def test_plain_message_prints_once(capsys):
    channel = _make_channel()
    event = OutboundEvent.text_reply(channel="cli", chat_id="cli", text="hello")
    await channel.send(event)
    assert capsys.readouterr().out.count("hello") == 1


@pytest.mark.asyncio
async def test_stream_final_prints_only_remainder(capsys):
    channel = _make_channel()
    await channel.send(_stream_event("你好，", final=False))
    await channel.send(_stream_event("我是 Echo。", final=False))
    # Final carries the FULL text (for edit-capable channels)
    await channel.send(_stream_event("你好，我是 Echo。很高兴见到你。", final=True, full_text=True))
    out = capsys.readouterr().out
    assert out.count("你好，我是 Echo。") == 1  # not duplicated
    assert "很高兴见到你。" in out  # remainder still printed


@pytest.mark.asyncio
async def test_stream_final_without_chunks_prints_full_text(capsys):
    channel = _make_channel()
    await channel.send(_stream_event("完整回复内容", final=True, full_text=True))
    assert capsys.readouterr().out.count("完整回复内容") == 1


@pytest.mark.asyncio
async def test_diverged_final_reprints_cleanly(capsys):
    channel = _make_channel()
    await channel.send(_stream_event("草稿内容", final=False))
    await channel.send(_stream_event("最终修订内容", final=True, full_text=True))
    out = capsys.readouterr().out
    assert "最终修订内容" in out


@pytest.mark.asyncio
async def test_stream_buffer_bounded(capsys):
    channel = _make_channel()
    for i in range(50):
        await channel.send(_stream_event("x", final=False, event_id=f"ev{i}"))
    assert len(channel._stream_printed) <= channel._max_stream_entries
