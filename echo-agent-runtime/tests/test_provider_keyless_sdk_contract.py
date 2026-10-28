"""A keyless local endpoint must remain constructible with the real SDK."""

import json
import os

import httpx
import openai
import pytest

from echo_agent.config.schema import ProviderConfig
from echo_agent.models.providers import create_provider


@pytest.mark.asyncio
async def test_keyless_compatible_provider_real_sdk_tool_roundtrip(monkeypatch):
    for name in ("OPENAI_API_KEY", "OPENAI_ADMIN_KEY"):
        monkeypatch.delenv(name, raising=False)
    seen = []

    def endpoint(request):
        body = json.loads(request.content)
        seen.append(body)
        assert request.url.path == "/v1/chat/completions"
        assert request.headers["authorization"] == "Bearer not-required"
        if len(seen) == 1:
            message = {"role": "assistant", "content": None, "tool_calls": [{
                "id": "call1", "type": "function", "function": {"name": "read_file", "arguments": '{"path":"note.txt"}'},
            }]}
            finish = "tool_calls"
        else:
            assert body["messages"][-1] == {"role": "tool", "tool_call_id": "call1", "content": "hello"}
            message, finish = {"role": "assistant", "content": "Read hello."}, "stop"
        return httpx.Response(200, json={
            "id": "completion1", "object": "chat.completion", "created": 1, "model": "local-model",
            "choices": [{"index": 0, "message": message, "finish_reason": finish}],
            "usage": {"prompt_tokens": 2, "completion_tokens": 1, "total_tokens": 3},
        })

    sdk = openai.AsyncOpenAI

    def make_sdk(**kwargs):
        return sdk(**kwargs, http_client=httpx.AsyncClient(transport=httpx.MockTransport(endpoint)))

    monkeypatch.setattr(openai, "AsyncOpenAI", make_sdk)
    config = ProviderConfig(name="openai", api_base="http://local-test.invalid/v1", models=["local-model"])
    provider = create_provider(config)
    try:
        messages = [{"role": "user", "content": "read note.txt"}]
        result = await provider.chat_with_retry(messages=messages)
        assert result.tool_calls[0].arguments == {"path": "note.txt"}
        messages += [
            {"role": "assistant", "tool_calls": [result.tool_calls[0].to_openai_format()]},
            {"role": "tool", "tool_call_id": "call1", "content": "hello"},
        ]
        final = await provider.chat_with_retry(messages=messages)
        assert final.content == "Read hello."
        assert final.finish_reason == "stop"
        assert config.api_key == ""
        assert "OPENAI_API_KEY" not in os.environ
    finally:
        await provider.aclose()


def test_default_openai_still_requires_credentials(monkeypatch):
    monkeypatch.delenv("OPENAI_API_KEY", raising=False)
    with pytest.raises(ValueError, match="requires api_key"):
        create_provider(ProviderConfig(name="openai", models=["gpt-test"]))
