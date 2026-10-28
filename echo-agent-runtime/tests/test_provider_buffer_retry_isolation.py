"""A final retry must not deliver buffered draft text from a failed attempt."""

import pytest

from echo_agent.models.provider import LLMProvider, LLMResponse


@pytest.mark.asyncio
async def test_last_stream_retry_delivers_only_its_own_buffer():
    class Provider(LLMProvider):
        calls = 0

        def get_default_model(self):
            return "test-model"

        async def chat(self, **kwargs):
            raise AssertionError("native streaming is supported")

        def _retry_delays(self):
            return [0]

        async def chat_stream(self, **kwargs):
            self.calls += 1
            if kwargs.get("on_reasoning"):
                await kwargs["on_reasoning"]("one displayed thought")
            if self.calls == 1:
                await kwargs["on_delta"]("discarded draft / ")
                return LLMResponse(content="429 rate limit", finish_reason="error")
            await kwargs["on_delta"]("authoritative answer")
            return LLMResponse(content="authoritative answer")

    delivered = []
    reasoning = []
    provider = Provider()
    result = await provider.chat_stream_with_retry(
        messages=[{"role": "user", "content": "hello"}], tools=[],
        on_delta=delivered.append,
        on_reasoning=reasoning.append,
    )
    assert result.content == "authoritative answer"
    assert provider.calls == 2
    assert delivered == [result.content]
    assert reasoning == ["one displayed thought"]
