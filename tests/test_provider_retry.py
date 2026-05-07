"""Tests for LLMProvider retry logic with jitter."""

from __future__ import annotations

import asyncio
from unittest.mock import AsyncMock, patch

import pytest

from echo_agent.models.provider import LLMProvider, LLMResponse


class _TestProvider(LLMProvider):
    def __init__(self):
        super().__init__()
        self.chat_mock = AsyncMock()

    async def chat(self, messages, tools=None, model=None, tool_choice=None, **kwargs):
        return await self.chat_mock(messages, tools, model, tool_choice, **kwargs)

    def get_default_model(self):
        return "test-model"


@pytest.mark.asyncio
async def test_chat_with_retry_success_first_attempt() -> None:
    provider = _TestProvider()
    provider.chat_mock.return_value = LLMResponse(content="hello", finish_reason="stop")

    result = await provider.chat_with_retry(messages=[{"role": "user", "content": "hi"}])

    assert result.content == "hello"
    assert result.finish_reason == "stop"
    assert provider.chat_mock.call_count == 1


@pytest.mark.asyncio
async def test_chat_with_retry_transient_then_success() -> None:
    provider = _TestProvider()
    provider.chat_mock.side_effect = [
        LLMResponse(content="Error: 429 rate limit", finish_reason="error"),
        LLMResponse(content="success", finish_reason="stop"),
    ]

    with patch("asyncio.sleep", new_callable=AsyncMock):
        result = await provider.chat_with_retry(messages=[{"role": "user", "content": "hi"}])

    assert result.content == "success"
    assert provider.chat_mock.call_count == 2


@pytest.mark.asyncio
async def test_chat_with_retry_non_transient_no_retry() -> None:
    provider = _TestProvider()
    provider.chat_mock.return_value = LLMResponse(content="Error: invalid api key", finish_reason="error")

    result = await provider.chat_with_retry(messages=[{"role": "user", "content": "hi"}])

    assert result.finish_reason == "error"
    assert provider.chat_mock.call_count == 1


@pytest.mark.asyncio
async def test_chat_with_retry_all_fail_returns_last_error() -> None:
    provider = _TestProvider()
    provider.chat_mock.return_value = LLMResponse(content="Error: 503 overloaded", finish_reason="error")

    with patch("asyncio.sleep", new_callable=AsyncMock):
        result = await provider.chat_with_retry(messages=[{"role": "user", "content": "hi"}])

    assert result.finish_reason == "error"
    assert "503" in (result.content or "")
    assert provider.chat_mock.call_count == 4  # 3 retries + 1 final


@pytest.mark.asyncio
async def test_chat_with_retry_jitter_applied() -> None:
    provider = _TestProvider()
    provider.chat_mock.return_value = LLMResponse(content="Error: 429 rate limit", finish_reason="error")
    sleep_calls: list[float] = []

    async def mock_sleep(duration: float) -> None:
        sleep_calls.append(duration)

    with patch("asyncio.sleep", side_effect=mock_sleep):
        await provider.chat_with_retry(messages=[{"role": "user", "content": "hi"}])

    assert len(sleep_calls) == 3
    for i, base in enumerate([1, 2, 4]):
        assert base * 0.5 <= sleep_calls[i] <= base * 1.5


@pytest.mark.asyncio
async def test_chat_stream_with_retry_no_retry_after_emit() -> None:
    provider = _TestProvider()
    deltas: list[str] = []

    async def fake_stream(messages, tools=None, model=None, tool_choice=None, on_delta=None, **kw):
        if on_delta:
            await on_delta("partial")
        return LLMResponse(content="Error: 500 server error", finish_reason="error")

    provider.chat_stream = AsyncMock(side_effect=fake_stream)

    result = await provider.chat_stream_with_retry(
        messages=[{"role": "user", "content": "hi"}],
        on_delta=lambda d: deltas.append(d),
    )

    assert result.finish_reason == "error"
    assert provider.chat_stream.call_count == 1  # no retry after emit


@pytest.mark.asyncio
async def test_chat_stream_with_retry_retry_before_emit() -> None:
    provider = _TestProvider()
    call_count = 0

    async def fake_stream(messages, tools=None, model=None, tool_choice=None, on_delta=None, **kw):
        nonlocal call_count
        call_count += 1
        if call_count == 1:
            return LLMResponse(content="Error: 502 bad gateway", finish_reason="error")
        return LLMResponse(content="success", finish_reason="stop")

    provider.chat_stream = AsyncMock(side_effect=fake_stream)

    with patch("asyncio.sleep", new_callable=AsyncMock):
        result = await provider.chat_stream_with_retry(
            messages=[{"role": "user", "content": "hi"}],
        )

    assert result.content == "success"
    assert call_count == 2
