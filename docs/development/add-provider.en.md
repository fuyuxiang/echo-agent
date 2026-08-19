# Adding a Provider

This guide explains how to integrate a new LLM service provider (e.g., Mistral, Cohere, local models) into Echo Agent.

## Architecture Overview

```
echo_agent/models/
├── provider.py          # LLMProvider abstract base class
├── providers/
│   ├── __init__.py      # Factory function + _PROVIDER_MAP registry
│   ├── openai_provider.py   # Reference implementation
│   └── your_provider.py     # ← Your new Provider
```

All Providers inherit from `LLMProvider` and implement `chat()` and `get_default_model()`. The system routes to concrete implementations via the `_PROVIDER_MAP` dictionary by name.

## Step 1: Create the Provider Class

Create a new file under `echo_agent/models/providers/`, e.g., `mistral_provider.py`:

```python
"""Mistral provider — chat completions via the Mistral SDK."""

from __future__ import annotations

from typing import Any

from loguru import logger

from echo_agent.models.provider import (
    LLMProvider,
    LLMResponse,
    StreamDeltaCallback,
    StreamReasoningCallback,
    ToolCallRequest,
    _invoke_stream_callback,
)


class MistralProvider(LLMProvider):

    def __init__(self, api_key: str = "", api_base: str = "", default_model: str = "", **kwargs: Any):
        super().__init__(api_key=api_key, api_base=api_base)
        self._default_model = default_model or "mistral-large-latest"
        self._client = self._build_client()

    def _build_client(self) -> Any:
        try:
            from mistralai import Mistral
        except ImportError:
            raise ImportError("mistral SDK required: pip install mistralai")
        return Mistral(api_key=self.api_key)

    async def chat(
        self,
        messages: list[dict[str, Any]],
        tools: list[dict[str, Any]] | None = None,
        model: str | None = None,
        tool_choice: str | dict | None = None,
        **kwargs: Any,
    ) -> LLMResponse:
        """Send a chat completion request."""
        params: dict[str, Any] = {
            "model": model or self._default_model,
            "messages": messages,
        }
        if tools:
            params["tools"] = tools
        if tool_choice:
            params["tool_choice"] = tool_choice

        try:
            resp = await self._client.chat.complete_async(**params)
        except Exception as e:
            logger.error("Mistral API error: {}", e)
            return LLMResponse(content=f"Error: {e}", finish_reason="error")

        return self._parse_response(resp)

    def get_default_model(self) -> str:
        return self._default_model

    async def chat_stream(
        self,
        messages: list[dict[str, Any]],
        tools: list[dict[str, Any]] | None = None,
        model: str | None = None,
        tool_choice: str | dict | None = None,
        on_delta: StreamDeltaCallback | None = None,
        on_reasoning: StreamReasoningCallback | None = None,
        **kwargs: Any,
    ) -> LLMResponse:
        """Streaming chat completion."""
        # Implement streaming response...
        pass

    def _parse_response(self, resp: Any) -> LLMResponse:
        """Convert SDK response to unified LLMResponse."""
        choice = resp.choices[0]
        tool_calls = []
        if choice.message.tool_calls:
            for tc in choice.message.tool_calls:
                tool_calls.append(ToolCallRequest(
                    id=tc.id,
                    name=tc.function.name,
                    arguments=tc.function.arguments if isinstance(tc.function.arguments, dict)
                              else {},
                ))
        return LLMResponse(
            content=choice.message.content or "",
            tool_calls=tool_calls,
            finish_reason=choice.finish_reason or "stop",
            usage={
                "input_tokens": resp.usage.prompt_tokens,
                "output_tokens": resp.usage.completion_tokens,
            },
            model=resp.model or self._default_model,
        )

    async def embed(self, text: str, model: str | None = None) -> list[float] | None:
        """Optional: implement embedding interface."""
        return None
```

## Step 2: Register in the Provider Map

Edit `echo_agent/models/providers/__init__.py` and add the mapping to `_PROVIDER_MAP`:

```python
_PROVIDER_MAP: dict[str, str] = {
    "openai": "echo_agent.models.providers.openai_provider.OpenAIProvider",
    "anthropic": "echo_agent.models.providers.anthropic_provider.AnthropicProvider",
    "bedrock": "echo_agent.models.providers.bedrock_provider.BedrockProvider",
    "aws": "echo_agent.models.providers.bedrock_provider.BedrockProvider",
    "gemini": "echo_agent.models.providers.gemini_provider.GeminiProvider",
    "google": "echo_agent.models.providers.gemini_provider.GeminiProvider",
    "openrouter": "echo_agent.models.providers.openrouter_provider.OpenRouterProvider",
    # ← Add here
    "mistral": "echo_agent.models.providers.mistral_provider.MistralProvider",
}
```

If the provider uses an environment variable for API key discovery, also add to `_API_KEY_ENV`:

```python
_API_KEY_ENV: dict[str, tuple[str, ...]] = {
    ...
    "mistral": ("MISTRAL_API_KEY",),
}
```

## Step 3: Add Optional Dependency

Declare in `pyproject.toml`:

```toml
[project.optional-dependencies]
mistral = ["mistralai>=1.0"]
```

Also add it to the `all` and `allproviders` collections.

## Step 4: Write Tests

Create a test file under `tests/`:

```python
"""tests/test_mistral_provider.py"""

import pytest
from unittest.mock import AsyncMock, patch, MagicMock

from echo_agent.models.providers.mistral_provider import MistralProvider


@pytest.fixture
def provider():
    with patch("echo_agent.models.providers.mistral_provider.MistralProvider._build_client"):
        p = MistralProvider(api_key="test-key")
        p._client = MagicMock()
        return p


@pytest.mark.asyncio
async def test_chat_success(provider):
    mock_resp = MagicMock()
    mock_resp.choices = [MagicMock()]
    mock_resp.choices[0].message.content = "Hello!"
    mock_resp.choices[0].message.tool_calls = None
    mock_resp.choices[0].finish_reason = "stop"
    mock_resp.usage.prompt_tokens = 10
    mock_resp.usage.completion_tokens = 5
    mock_resp.model = "mistral-large-latest"

    provider._client.chat.complete_async = AsyncMock(return_value=mock_resp)

    result = await provider.chat([{"role": "user", "content": "Hi"}])
    assert result.content == "Hello!"
    assert result.finish_reason == "stop"


@pytest.mark.asyncio
async def test_chat_error(provider):
    provider._client.chat.complete_async = AsyncMock(side_effect=Exception("API down"))
    result = await provider.chat([{"role": "user", "content": "Hi"}])
    assert result.finish_reason == "error"
```

## Required Methods

| Method | Required | Description |
|--------|----------|-------------|
| `chat()` | Yes | Non-streaming chat completion |
| `get_default_model()` | Yes | Return default model identifier |
| `chat_stream()` | No | Streaming response (auto-degrades to non-streaming if not implemented) |
| `embed()` | No | Embedding vector generation |
| `supports_embed()` | No | Declare embedding support |
| `aclose()` | No | Clean up SDK client resources |

## LLMResponse Field Specification

```python
@dataclass
class LLMResponse:
    content: str | None = None          # Text response
    tool_calls: list[ToolCallRequest]   # Tool call requests
    finish_reason: str = "stop"         # stop / tool_calls / error / length
    usage: dict[str, int] = {}          # input_tokens, output_tokens, cache_read_input_tokens
    model: str = ""                     # Actual model used
    reasoning_content: str | None = None  # Reasoning content (e.g., Claude's thinking)
```

## Checklist

- [ ] Inherit `LLMProvider`, implement `chat()` + `get_default_model()`
- [ ] Register in `_PROVIDER_MAP`
- [ ] Register in `_API_KEY_ENV` (if applicable)
- [ ] Add optional dependency to `pyproject.toml`
- [ ] SDK imported lazily (`ImportError` with install instructions)
- [ ] Error handling: API exceptions return `finish_reason="error"`, never throw
- [ ] Write unit tests (mock SDK calls)
- [ ] Tool calls correctly converted to `ToolCallRequest` format
- [ ] Usage statistics properly populated (affects cost tracking)

!!! question "Pending maintainer confirmation"
    Should the OpenAI-compatible mode (providers not in `_PROVIDER_MAP` automatically degrade to OpenAI SDK calls) be documented as a formal "custom Provider" entry point?
