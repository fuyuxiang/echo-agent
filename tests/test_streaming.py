"""Tests for TokenStreamPublisher — adaptive streaming with boundary-aware flushing."""

from unittest.mock import AsyncMock, MagicMock

import pytest

from echo_agent.agent.streaming import TokenStreamPublisher


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
        assert result is False
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
        assert result is True
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
        assert result is True
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
