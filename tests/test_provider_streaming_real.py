import pytest
from echo_agent.models.provider import LLMProvider, StreamingUnsupported, LLMResponse


class _NoStreamProvider(LLMProvider):
    """只实现 chat，不实现 chat_stream — 走基类默认。"""
    def __init__(self):
        super().__init__(api_key="k", api_base="b")
        self.chat_calls = 0

    async def chat(self, messages, tools=None, model=None, tool_choice=None, **kwargs):
        self.chat_calls += 1
        return LLMResponse(content="full answer", finish_reason="stop")

    def get_default_model(self):
        return "stub"


@pytest.mark.asyncio
async def test_base_chat_stream_raises_unsupported():
    p = _NoStreamProvider()
    with pytest.raises(StreamingUnsupported):
        await p.chat_stream(messages=[{"role": "user", "content": "hi"}])


@pytest.mark.asyncio
async def test_chat_stream_with_retry_falls_back_to_chat():
    p = _NoStreamProvider()
    deltas = []
    resp = await p.chat_stream_with_retry(
        messages=[{"role": "user", "content": "hi"}],
        on_delta=lambda d: deltas.append(d),
    )
    assert resp.content == "full answer"
    assert p.chat_calls >= 1  # 确实降级到了 chat 路径


from echo_agent.models.providers.anthropic_provider import AnthropicProvider


@pytest.mark.asyncio
async def test_anthropic_chat_stream_emits_deltas_before_completion(monkeypatch):
    # 用一个产出两段 text delta 的假 stream，断言 on_delta 在拿到 final 前被调用
    deltas = []

    class _Final:
        content = [type("B", (), {"type": "text", "text": "hello world"})()]
        stop_reason = "end_turn"
        usage = type("U", (), {"input_tokens": 1, "output_tokens": 2})()
        model = "claude-x"

    p = AnthropicProvider.__new__(AnthropicProvider)
    p._default_model = "claude-x"
    p._enable_cache = False
    p._thinking_effort = ""
    from echo_agent.models.provider import GenerationParams
    p.generation = GenerationParams()

    class _Stream:
        async def __aenter__(self_inner): return self_inner
        async def __aexit__(self_inner, *a): return False
        def __aiter__(self_inner): return _aiter_events()
        async def get_final_message(self_inner): return _Final()

    async def _aiter_events():
        # 模拟 content_block_delta 文本事件
        for t in ("hello ", "world"):
            yield type("E", (), {"type": "content_block_delta",
                                 "delta": type("D", (), {"type": "text_delta", "text": t})()})()

    p._client = type("C", (), {"messages": type("M", (), {"stream": staticmethod(lambda **k: _Stream())})()})()

    resp = await p.chat_stream(messages=[{"role": "user", "content": "hi"}],
                               on_delta=lambda d: deltas.append(d))
    assert "".join(deltas) == "hello world"
    assert resp.content == "hello world"
