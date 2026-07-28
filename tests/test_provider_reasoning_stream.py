"""推理增量的传递：provider 边收边转发，重试/换 key 不重放。

此前 reasoning 只能在调用结束后从 LLMResponse.reasoning_content 整段读出，
思考型模型的等待期里客户端没有任何可显示的内容。加上 on_reasoning 之后，关键
风险变成重复：重试与凭据轮转会让模型从头再想一遍，若照样转发，客户端会把第二
段推理追加到第一段后面，读起来是一段前后矛盾的思考。
"""

from __future__ import annotations

from unittest.mock import AsyncMock, MagicMock, patch

import pytest

from echo_agent.models.provider import LLMProvider, LLMResponse


class _Chunk:
    def __init__(self, content=None, reasoning=None, finish_reason=None):
        delta = MagicMock(spec=["content", "reasoning_content", "tool_calls"])
        delta.content = content
        delta.reasoning_content = reasoning
        delta.tool_calls = None
        choice = MagicMock()
        choice.delta = delta
        choice.finish_reason = finish_reason
        self.choices = [choice]
        self.model = "gpt-5.5"
        self.usage = None


class _Stream:
    def __init__(self, chunks):
        self._chunks = chunks

    def __aiter__(self):
        async def gen():
            for c in self._chunks:
                yield c
        return gen()


def _openai():
    from echo_agent.models.providers.openai_provider import OpenAIProvider
    with patch.object(OpenAIProvider, "_build_client", return_value=MagicMock()):
        return OpenAIProvider(api_key="x", default_model="gpt-5.5")


@pytest.mark.asyncio
async def test_openai_forwards_reasoning_deltas_as_they_arrive():
    provider = _openai()
    provider._client.chat.completions.create = AsyncMock(return_value=_Stream([
        _Chunk(reasoning="先"),
        _Chunk(reasoning="想想"),
        _Chunk(content="答案"),
        _Chunk(finish_reason="stop"),
    ]))
    seen: list[str] = []
    resp = await provider.chat_stream(
        messages=[{"role": "user", "content": "hi"}],
        on_reasoning=lambda d: seen.append(d) and None,
    )
    assert seen == ["先", "想想"]
    # 转发之后仍然累积，未订阅回调的调用方要靠响应上的整段推理
    assert resp.reasoning_content == "先想想"


@pytest.mark.asyncio
async def test_openai_keeps_reasoning_off_the_answer_channel():
    provider = _openai()
    provider._client.chat.completions.create = AsyncMock(return_value=_Stream([
        _Chunk(reasoning="思考"),
        _Chunk(content="答案"),
        _Chunk(finish_reason="stop"),
    ]))
    answer: list[str] = []
    await provider.chat_stream(
        messages=[{"role": "user", "content": "hi"}],
        on_delta=lambda d: answer.append(d) and None,
    )
    assert answer == ["答案"]


class _Block:
    def __init__(self, type_, **kw):
        self.type = type_
        for k, v in kw.items():
            setattr(self, k, v)


class _Msg:
    def __init__(self, content):
        self.content = content
        self.stop_reason = "end_turn"
        self.usage = None
        self.model = "claude-opus-5"


def test_anthropic_thinking_blocks_become_reasoning_content():
    """扩展思考在 Claude 这边是一种 content block，不是单独字段。
    之前解析器只认 text/tool_use，思考整段被丢掉：调用方拿不到 trace，配合流式
    还分不清"没有推理"和"推理被提升成了答案"（后者要撤回已显示的行）。"""
    from echo_agent.models.providers.anthropic_provider import parse_anthropic_message

    resp = parse_anthropic_message(_Msg([
        _Block("thinking", thinking="先看配置"),
        _Block("text", text="精排没生效是因为开关是关的"),
    ]))
    assert resp.reasoning_content == "先看配置"
    assert resp.content == "精排没生效是因为开关是关的"


def test_anthropic_response_without_thinking_has_no_reasoning():
    from echo_agent.models.providers.anthropic_provider import parse_anthropic_message

    resp = parse_anthropic_message(_Msg([_Block("text", text="答案")]))
    assert resp.reasoning_content is None


class _Ev:
    def __init__(self, delta):
        self.type = "content_block_delta"
        self.delta = delta


class _AStream:
    """模拟 anthropic SDK 的 messages.stream 上下文管理器。"""

    def __init__(self, events, final):
        self._events = events
        self._final = final

    async def __aenter__(self):
        return self

    async def __aexit__(self, *exc):
        return False

    def __aiter__(self):
        async def gen():
            for e in self._events:
                yield e
        return gen()

    async def get_final_message(self):
        return self._final


@pytest.mark.asyncio
async def test_anthropic_stream_forwards_thinking_deltas():
    from echo_agent.models.providers.anthropic_provider import (
        stream_anthropic_messages,
    )

    final = _Msg([_Block("thinking", thinking="先看配置"), _Block("text", text="答案")])
    events = [
        _Ev(_Block("thinking_delta", thinking="先看")),
        _Ev(_Block("thinking_delta", thinking="配置")),
        _Ev(_Block("text_delta", text="答案")),
        _Ev(None),  # SDK 会发不带 delta 的事件，不能因此中断整个流
    ]
    client = MagicMock()
    client.messages.stream = MagicMock(return_value=_AStream(events, final))

    thoughts: list[str] = []
    answer: list[str] = []
    await stream_anthropic_messages(
        client, {"model": "m"},
        lambda d: answer.append(d) and None,
        lambda d: thoughts.append(d) and None,
    )
    assert thoughts == ["先看", "配置"]
    assert answer == ["答案"]


class _RetryProvider(LLMProvider):
    """第一次调用报错、第二次成功，两次都产出推理。"""

    def __init__(self):
        super().__init__()
        self.calls = 0

    def get_default_model(self) -> str:
        return "m"

    async def chat(self, messages, tools=None, model=None, tool_choice=None, **kw):
        return LLMResponse(content="x")

    async def chat_stream(
        self, messages, tools=None, model=None, tool_choice=None,
        on_delta=None, on_reasoning=None, **kw,
    ):
        self.calls += 1
        if on_reasoning:
            await on_reasoning(f"第{self.calls}次推理")
        if self.calls == 1:
            return LLMResponse(content="Error: rate limit", finish_reason="error")
        return LLMResponse(content="答案", reasoning_content="第2次推理")


@pytest.mark.asyncio
async def test_a_retry_does_not_replay_reasoning():
    provider = _RetryProvider()
    provider.max_retries = 2
    provider._retry_delays = lambda: [0.0, 0.0]
    seen: list[str] = []
    resp = await provider.chat_stream_with_retry(
        messages=[{"role": "user", "content": "hi"}],
        on_reasoning=lambda d: seen.append(d) and None,
    )
    assert provider.calls == 2
    assert resp.content == "答案"
    # 只保留首次尝试的推理；成功那次的整段文本仍在响应上
    assert seen == ["第1次推理"]


class _EmptyThenGoodProvider(LLMProvider):
    """空成功 -> 内层重试。空回复的那次也产出了推理。"""

    def __init__(self):
        super().__init__()
        self.calls = 0

    def get_default_model(self) -> str:
        return "m"

    async def chat(self, messages, tools=None, model=None, tool_choice=None, **kw):
        return LLMResponse(content="x")

    async def chat_stream(
        self, messages, tools=None, model=None, tool_choice=None,
        on_delta=None, on_reasoning=None, **kw,
    ):
        self.calls += 1
        if on_reasoning:
            await on_reasoning(f"推理{self.calls}")
        if self.calls == 1:
            return LLMResponse(content="", finish_reason="stop")
        return LLMResponse(content="真答案", finish_reason="stop")


@pytest.mark.asyncio
async def test_the_empty_success_retry_does_not_replay_reasoning():
    provider = _EmptyThenGoodProvider()
    provider.max_retries = 2
    provider._retry_delays = lambda: [0.0, 0.0]
    seen: list[str] = []
    resp = await provider.chat_stream_with_retry(
        messages=[{"role": "user", "content": "hi"}],
        on_reasoning=lambda d: seen.append(d) and None,
    )
    assert resp.content == "真答案"
    assert seen == ["推理1"]


@pytest.mark.asyncio
async def test_a_provider_without_reasoning_still_streams_the_answer():
    provider = _EmptyThenGoodProvider()
    provider.max_retries = 1
    provider._retry_delays = lambda: [0.0]
    # 不传 on_reasoning 时行为与加这条通道之前一致
    resp = await provider.chat_stream_with_retry(
        messages=[{"role": "user", "content": "hi"}],
    )
    assert resp.content == "真答案"
