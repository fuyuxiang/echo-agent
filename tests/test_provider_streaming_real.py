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
