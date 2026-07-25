"""Tests for TokenStreamPublisher — adaptive streaming with boundary-aware flushing."""

from unittest.mock import AsyncMock, MagicMock

import pytest

from echo_agent.agent.streaming import TokenStreamPublisher
from echo_agent.bus.delivery import DeliveryStage


def _make_event():
    """Create a minimal InboundEvent-like mock."""
    event = MagicMock()
    event.channel = "test"
    event.chat_id = "chat_1"
    event.reply_to_id = None
    event.event_id = "evt_001"
    event.metadata = {}
    return event


def _make_publisher(*, enabled=True, flush_chars=50, flush_interval_ms=100, paragraph_mode=False, intro_text=""):
    bus = AsyncMock()
    event = _make_event()
    pub = TokenStreamPublisher(
        bus=bus,
        event=event,
        enabled=enabled,
        flush_chars=flush_chars,
        flush_interval_ms=flush_interval_ms,
        paragraph_mode=paragraph_mode,
        intro_text=intro_text,
    )
    return pub, bus


class TestTokenStreamPublisherDisabled:
    """When enabled=False, all methods are no-op."""

    @pytest.mark.asyncio
    async def test_start_noop(self):
        pub, bus = _make_publisher(enabled=False, intro_text="Hello")
        await pub.start()
        assert pub._full_text == ""
        bus.publish_outbound.assert_not_called()

    @pytest.mark.asyncio
    async def test_on_delta_noop(self):
        pub, bus = _make_publisher(enabled=False)
        await pub.on_delta("some text")
        assert pub._full_text == ""
        bus.publish_outbound.assert_not_called()

    @pytest.mark.asyncio
    async def test_finalize_noop(self):
        pub, bus = _make_publisher(enabled=False)
        result = await pub.finalize("final text")
        # Disabled streaming reports NO_HANDLER so callers fall back to a plain send.
        assert result.stage is DeliveryStage.NO_HANDLER
        bus.publish_outbound.assert_not_called()


class TestTokenStreamPublisherStart:
    """start() with intro_text sets full_text."""

    @pytest.mark.asyncio
    async def test_start_with_intro_text(self):
        pub, bus = _make_publisher(enabled=True, intro_text="Welcome!")
        await pub.start()
        assert pub._full_text == "Welcome!"
        assert pub._pending == "Welcome!"

    @pytest.mark.asyncio
    async def test_start_without_intro_text(self):
        pub, bus = _make_publisher(enabled=True, intro_text="")
        await pub.start()
        assert pub._full_text == ""


class TestTokenStreamPublisherOnDelta:
    """on_delta accumulates text."""

    @pytest.mark.asyncio
    async def test_accumulates_text(self):
        pub, bus = _make_publisher(enabled=True, flush_chars=1000, flush_interval_ms=10000)
        await pub.on_delta("Hello ")
        await pub.on_delta("World")
        assert pub._full_text == "Hello World"
        assert pub._pending == "Hello World"

    @pytest.mark.asyncio
    async def test_intro_separator_added(self):
        pub, bus = _make_publisher(enabled=True, intro_text="Intro", flush_chars=1000, flush_interval_ms=10000)
        await pub.start()
        await pub.on_delta("Body")
        assert pub._full_text == "Intro\n\nBody"

    @pytest.mark.asyncio
    async def test_empty_delta_ignored(self):
        pub, bus = _make_publisher(enabled=True)
        await pub.on_delta("")
        assert pub._full_text == ""


class TestTokenStreamPublisherFinalize:
    """finalize publishes the final message."""

    @pytest.mark.asyncio
    async def test_finalize_sent_nonfinal_true(self):
        pub, bus = _make_publisher(enabled=True, flush_chars=10, flush_interval_ms=50, paragraph_mode=False)
        # Force a non-final flush by sending enough text
        pub._sent_nonfinal = True
        pub._full_text = "partial"
        pub._pending = ""

        result = await pub.finalize("partial complete")
        # finalize now transparently returns publish_outbound's receipt.
        assert result is bus.publish_outbound.return_value
        # Should publish the full text as final
        bus.publish_outbound.assert_called()
        call_args = bus.publish_outbound.call_args[0][0]
        assert call_args.is_final is True

    @pytest.mark.asyncio
    async def test_finalize_sent_nonfinal_false(self):
        pub, bus = _make_publisher(enabled=True, flush_chars=1000, flush_interval_ms=10000)
        # No non-final was sent yet
        assert pub._sent_nonfinal is False
        result = await pub.finalize("complete text")
        assert result is bus.publish_outbound.return_value
        bus.publish_outbound.assert_called_once()
        call_args = bus.publish_outbound.call_args[0][0]
        assert call_args.is_final is True


class TestTokenStreamPublisherCodeBlock:
    """Code blocks should not trigger flush on paragraph boundaries."""

    @pytest.mark.asyncio
    async def test_code_block_suppresses_paragraph_flush(self):
        pub, bus = _make_publisher(
            enabled=True, flush_chars=120, flush_interval_ms=1200, paragraph_mode=True
        )
        # Enter code block
        await pub.on_delta("```python\n")
        assert pub._in_code_block is True
        # Even with paragraph break inside code, no early flush if below threshold
        await pub.on_delta("x = 1\n\ny = 2\n")
        # Code block content should not have triggered a paragraph flush
        # (it only flushes when pending >= flush_chars * 3 inside code)
        assert "```python\n" in pub._full_text


class TestTokenStreamPublisherDiscard:
    """discard() 撤回乐观流式发出的工具前草稿。"""

    @pytest.mark.asyncio
    async def test_discard_clears_streamed_state(self):
        pub, _ = _make_publisher(enabled=True, flush_chars=5, flush_interval_ms=1)
        await pub.on_delta("let me check the weather")
        assert pub._full_text
        await pub.discard()
        assert pub._full_text == ""
        assert pub._pending == ""
        assert pub._sent_nonfinal is False

    @pytest.mark.asyncio
    async def test_finalize_after_discard_sends_full_text_final(self):
        # 撤回后 finalize 必须走"从未流式"路径,整段全文一次性发出,
        # 而不是与被丢弃的草稿做增量 diff。
        pub, bus = _make_publisher(enabled=True, flush_chars=5, flush_interval_ms=1)
        await pub.on_delta("let me check")
        await pub.discard()
        bus.publish_outbound.reset_mock()
        await pub.finalize("It is sunny in Beijing.")
        bus.publish_outbound.assert_called_once()
        sent = bus.publish_outbound.call_args[0][0]
        assert sent.is_final is True
        assert sent.text == "It is sunny in Beijing."
        assert sent.metadata.get("_stream_full_text") is True

    @pytest.mark.asyncio
    async def test_discard_does_not_duplicate_intro(self):
        # response_stage 自己会把 intro 拼到 response_text 前面,
        # 所以 discard 不能把 intro 重新塞回缓冲,否则开场语会出现两次。
        pub, bus = _make_publisher(
            enabled=True, intro_text="Hi there", flush_chars=5, flush_interval_ms=1
        )
        await pub.start()
        await pub.on_delta("let me check")
        await pub.discard()
        assert pub._full_text == ""
        bus.publish_outbound.reset_mock()
        await pub.finalize("Hi there\n\nIt is sunny.")
        sent = bus.publish_outbound.call_args[0][0]
        assert sent.text.count("Hi there") == 1

    @pytest.mark.asyncio
    async def test_discard_noop_when_disabled(self):
        pub, bus = _make_publisher(enabled=False)
        await pub.discard()
        bus.publish_outbound.assert_not_called()
