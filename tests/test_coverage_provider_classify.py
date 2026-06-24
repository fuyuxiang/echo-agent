"""Characterization tests for LLMProvider 故障分类矩阵 + 重试循环。

补测覆盖缺口（优先级5，models/provider.py）：
- _status_code_of：从 SDK 异常对象提取 HTTP 状态码
- _classify_exception：HTTP 429/5xx→transient，4xx→permanent，其他异常→回退文本分类
- chat_with_retry：首次抛 transient 异常 → 重试后返回正常；全程 permanent → 不重试直接返回错误

性质：表征测试，以实际行为为准。
"""

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

    def get_default_model(self) -> str:
        return "test-model"


# ── _status_code_of ─────────────────────────────────────────────────────────


class _ExcWithStatusCode(Exception):
    def __init__(self, code: int):
        super().__init__("test")
        self.status_code = code


class _ExcWithResponse(Exception):
    def __init__(self, code: int):
        super().__init__("test")
        self.response = type("R", (), {"status_code": code})()


def test_status_code_of_direct_attribute() -> None:
    e = _ExcWithStatusCode(429)
    assert LLMProvider._status_code_of(e) == 429


def test_status_code_of_response_attribute() -> None:
    e = _ExcWithResponse(500)
    assert LLMProvider._status_code_of(e) == 500


def test_status_code_of_plain_exception_returns_none() -> None:
    assert LLMProvider._status_code_of(ValueError("no code")) is None


# ── _classify_exception ──────────────────────────────────────────────────────


def _make_provider() -> _TestProvider:
    p = _TestProvider()
    return p


def test_classify_exception_429_is_transient() -> None:
    p = _make_provider()
    assert p._classify_exception(_ExcWithStatusCode(429)) == "transient"


def test_classify_exception_500_is_transient() -> None:
    p = _make_provider()
    assert p._classify_exception(_ExcWithStatusCode(500)) == "transient"


def test_classify_exception_503_is_transient() -> None:
    p = _make_provider()
    assert p._classify_exception(_ExcWithStatusCode(503)) == "transient"


def test_classify_exception_400_is_permanent() -> None:
    p = _make_provider()
    assert p._classify_exception(_ExcWithStatusCode(400)) == "permanent"


def test_classify_exception_401_is_permanent() -> None:
    p = _make_provider()
    assert p._classify_exception(_ExcWithStatusCode(401)) == "permanent"


def test_classify_exception_403_is_permanent() -> None:
    p = _make_provider()
    assert p._classify_exception(_ExcWithStatusCode(403)) == "permanent"


def test_classify_exception_timeout_error_is_transient() -> None:
    p = _make_provider()
    assert p._classify_exception(TimeoutError("timed out")) == "transient"


def test_classify_exception_connection_error_is_transient() -> None:
    p = _make_provider()
    assert p._classify_exception(ConnectionError("reset")) == "transient"


def test_classify_exception_asyncio_timeout_is_transient() -> None:
    p = _make_provider()
    assert p._classify_exception(asyncio.TimeoutError()) == "transient"


def test_classify_exception_plain_exception_falls_back_to_text() -> None:
    p = _make_provider()
    # 无状态码、无类型匹配 → 回退文本分类；通用异常 → "unknown"
    assert p._classify_exception(RuntimeError("something went wrong")) == "unknown"


def test_classify_exception_text_rate_limit_is_transient() -> None:
    p = _make_provider()
    # 无状态码属性，但文本含 "rate limit" → transient
    assert p._classify_exception(RuntimeError("rate limit exceeded")) == "transient"


def test_classify_exception_text_unauthorized_is_permanent() -> None:
    p = _make_provider()
    assert p._classify_exception(RuntimeError("unauthorized request")) == "permanent"


# ── chat_with_retry：transient 异常 → 重试后成功 ─────────────────────────────


@pytest.mark.asyncio
async def test_chat_with_retry_transient_exception_then_success() -> None:
    """首次 chat() 抛 transient 异常（ConnectionError），第二次返回正常响应。"""
    provider = _TestProvider()
    provider.chat_mock.side_effect = [
        ConnectionError("connection reset"),
        LLMResponse(content="recovered", finish_reason="stop"),
    ]
    with patch("asyncio.sleep", new_callable=AsyncMock):
        result = await provider.chat_with_retry(messages=[{"role": "user", "content": "hi"}])

    assert result.content == "recovered"
    assert result.finish_reason == "stop"
    # 第一次失败、第二次成功
    assert provider.chat_mock.call_count == 2


@pytest.mark.asyncio
async def test_chat_with_retry_permanent_exception_no_retry() -> None:
    """chat() 抛 permanent 异常（401）→ 立即返回错误，不重试。"""
    provider = _TestProvider()
    provider.chat_mock.side_effect = _ExcWithStatusCode(401)

    result = await provider.chat_with_retry(messages=[{"role": "user", "content": "hi"}])

    # 永久错误立即返回，finish_reason="error"
    assert result.finish_reason == "error"
    assert provider.chat_mock.call_count == 1


@pytest.mark.asyncio
async def test_chat_with_retry_all_transient_exhausts_retries() -> None:
    """所有尝试均抛 transient 异常，最终返回最后一次错误响应。"""
    provider = _TestProvider()
    provider.chat_mock.side_effect = ConnectionError("reset")

    with patch("asyncio.sleep", new_callable=AsyncMock):
        result = await provider.chat_with_retry(messages=[{"role": "user", "content": "hi"}])

    assert result.finish_reason == "error"
    # max_retries=3 → 3次循环 + 1次最后尝试 = 4
    assert provider.chat_mock.call_count == 4
