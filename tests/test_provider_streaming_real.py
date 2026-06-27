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
    # 在假 stream 的 get_final_message 上打桩计数，验证真流式（边收边吐）时序：
    # 真流式下，首个 text delta 触发时 get_final_message 还没被调用；
    # 伪流式（攒完所有 event 后再 get_final_message 统一回吐）会在 delta 之前先取 final。
    final_calls = {"count": 0}
    final_called_at_first_delta = {"value": None}

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
        async def get_final_message(self_inner):
            final_calls["count"] += 1
            return _Final()

    async def _aiter_events():
        # 模拟 content_block_delta 文本事件
        for t in ("hello ", "world"):
            yield type("E", (), {"type": "content_block_delta",
                                 "delta": type("D", (), {"type": "text_delta", "text": t})()})()

    p._client = type("C", (), {"messages": type("M", (), {"stream": staticmethod(lambda **k: _Stream())})()})()

    def _on_delta(d):
        # 首次 delta 触发时，记录此刻 get_final_message 是否已被调用
        if final_called_at_first_delta["value"] is None:
            final_called_at_first_delta["value"] = final_calls["count"] > 0
        deltas.append(d)

    resp = await p.chat_stream(messages=[{"role": "user", "content": "hi"}],
                               on_delta=_on_delta)
    # 时序断言：真流式下首个 delta 触发时 final 尚未取到（边收边吐，而非攒完再吐）
    assert final_called_at_first_delta["value"] is False, \
        "delta 应严格早于 get_final_message（真流式），当前在 delta 之前已取 final（伪流式）"
    assert "".join(deltas) == "hello world"
    assert resp.content == "hello world"


@pytest.mark.asyncio
async def test_gemini_chat_stream_emits_deltas():
    from echo_agent.models.providers.gemini_provider import GeminiProvider
    deltas = []

    class _Chunk:
        def __init__(self, t): self.text = t
        candidates = []

    chunks = [_Chunk("foo"), _Chunk("bar")]

    class _FakeModel:
        def generate_content(self, **kw):
            assert kw.get("stream") is True
            return iter(chunks)

    p = GeminiProvider.__new__(GeminiProvider)
    p._default_model = "gemini-x"
    from echo_agent.models.provider import GenerationParams
    p.generation = GenerationParams()
    p._client = type("G", (), {"GenerativeModel": staticmethod(lambda **k: _FakeModel())})()
    # 让 _parse_response 收尾产出聚合文本
    p._parse_response = lambda resp, model_name: __import__("echo_agent.models.provider", fromlist=["LLMResponse"]).LLMResponse(content="foobar", finish_reason="stop")

    resp = await p.chat_stream(messages=[{"role": "user", "content": "hi"}],
                               on_delta=lambda d: deltas.append(d))
    assert "".join(deltas) == "foobar"


@pytest.mark.asyncio
async def test_gemini_aggregate_feeds_real_parse_response():
    """生产路径校验:真实 _GeminiAggregate 喂给真实 _parse_response,
    能正确还原跨 chunk 累积的文本、function_call 与 usage。"""
    from echo_agent.models.providers.gemini_provider import GeminiProvider, _GeminiAggregate

    class _Part:
        def __init__(self, text="", function_call=None):
            self.text = text
            self.function_call = function_call

    class _Content:
        def __init__(self, parts): self.parts = parts

    class _Candidate:
        def __init__(self, parts): self.content = _Content(parts)

    class _FC:
        def __init__(self, name, args):
            self.name = name
            self.args = args

    class _Usage:
        prompt_token_count = 7
        candidates_token_count = 11

    class _Chunk:
        def __init__(self, parts, usage=None):
            self.candidates = [_Candidate(parts)]
            if usage is not None:
                self.usage_metadata = usage

    # 文本分两 chunk 到达,function_call 在第三 chunk,usage 仅末块携带
    chunks = [
        _Chunk([_Part(text="Hello ")]),
        _Chunk([_Part(text="world")]),
        _Chunk([_Part(function_call=_FC("get_weather", {"city": "SF"}))], usage=_Usage()),
    ]

    agg = _GeminiAggregate(chunks)
    p = GeminiProvider.__new__(GeminiProvider)
    resp = p._parse_response(agg, "gemini-x")

    assert resp.content == "Hello world"
    assert resp.finish_reason == "tool_calls"
    assert len(resp.tool_calls) == 1
    assert resp.tool_calls[0].name == "get_weather"
    assert resp.tool_calls[0].arguments == {"city": "SF"}
    assert resp.usage["prompt_tokens"] == 7
    assert resp.usage["completion_tokens"] == 11
