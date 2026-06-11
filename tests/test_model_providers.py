"""Tests for model providers — StubProvider, TokenCounter, OpenAI, OpenRouter, Anthropic, Gemini, Bedrock."""

from __future__ import annotations

from unittest.mock import patch

import pytest

from echo_agent.models.provider import LLMResponse
from echo_agent.models.stub import StubProvider
from echo_agent.models.tokenizer import TokenCounter


# ══════════════════════════════════════════════════════════════════════════════
# StubProvider
# ══════════════════════════════════════════════════════════════════════════════


class TestStubProvider:
    def test_is_stub(self):
        provider = StubProvider(message="no LLM available")
        assert provider.is_stub is True

    @pytest.mark.asyncio
    async def test_chat_returns_message(self):
        provider = StubProvider(message="Provider unavailable")
        resp = await provider.chat(messages=[{"role": "user", "content": "hi"}])
        assert isinstance(resp, LLMResponse)
        assert resp.content == "Provider unavailable"

    def test_get_default_model(self):
        provider = StubProvider(message="x")
        assert provider.get_default_model() == "stub"

    @pytest.mark.asyncio
    async def test_notifies_only_once(self):
        provider = StubProvider(message="err")
        await provider.chat(messages=[])
        assert provider._notified is True
        # Second call should not re-log (just verify no exception)
        await provider.chat(messages=[])


# ══════════════════════════════════════════════════════════════════════════════
# TokenCounter
# ══════════════════════════════════════════════════════════════════════════════


class TestTokenCounter:
    def setup_method(self):
        # Clear cached instances between tests
        TokenCounter._instances.clear()

    def test_count_fallback_formula(self):
        counter = TokenCounter(provider="unknown", model="unknown")
        # Fallback: max(1, len(text) // 4)
        assert counter.count("") == 0
        assert counter.count("a") == 1  # max(1, 1//4) = 1
        assert counter.count("a" * 8) == 2  # max(1, 8//4) = 2
        assert counter.count("a" * 100) == 25

    def test_count_messages(self):
        counter = TokenCounter(provider="unknown", model="unknown")
        messages = [
            {"role": "user", "content": "Hello world"},
            {"role": "assistant", "content": "Hi there"},
        ]
        result = counter.count_messages(messages)
        # Each message: 4 overhead + count(content) + count(role)
        # Plus 2 conversation overhead
        assert result > 0
        assert isinstance(result, int)

    def test_count_messages_with_tool_calls(self):
        counter = TokenCounter(provider="unknown", model="unknown")
        messages = [
            {
                "role": "assistant",
                "content": "calling tool",
                "tool_calls": [
                    {"function": {"name": "exec", "arguments": '{"cmd": "ls"}'}}
                ],
            }
        ]
        result = counter.count_messages(messages)
        assert result > 0

    def test_for_model_caching(self):
        c1 = TokenCounter.for_model("unknown", "model-a")
        c2 = TokenCounter.for_model("unknown", "model-a")
        assert c1 is c2

    def test_for_model_different_keys(self):
        c1 = TokenCounter.for_model("unknown", "model-a")
        c2 = TokenCounter.for_model("unknown", "model-b")
        assert c1 is not c2


# ══════════════════════════════════════════════════════════════════════════════
# OpenAIProvider
# ══════════════════════════════════════════════════════════════════════════════


class TestOpenAIProvider:
    def _make_provider(self):
        with patch("echo_agent.models.providers.openai_provider.OpenAIProvider._build_client"):
            from echo_agent.models.providers.openai_provider import OpenAIProvider
            provider = OpenAIProvider(api_key="test-key", api_base="http://localhost", default_model="gpt-4")
        return provider

    def test_build_params_basic(self):
        provider = self._make_provider()
        messages = [{"role": "user", "content": "hi"}]
        params = provider._build_params(messages, None, None, None)
        assert params["model"] == "gpt-4"
        assert params["messages"] == [{"role": "user", "content": "hi"}]
        assert "temperature" in params
        assert "max_tokens" in params

    def test_build_params_with_tools(self):
        provider = self._make_provider()
        messages = [{"role": "user", "content": "hi"}]
        tools = [{"type": "function", "function": {"name": "test", "parameters": {}}}]
        params = provider._build_params(messages, tools, "gpt-3.5", "auto")
        assert params["model"] == "gpt-3.5"
        assert params["tools"] == tools
        assert params["tool_choice"] == "auto"

    def test_build_params_model_override(self):
        provider = self._make_provider()
        params = provider._build_params([{"role": "user", "content": "x"}], None, "custom-model", None)
        assert params["model"] == "custom-model"

    def test_clean_messages(self):
        provider = self._make_provider()
        messages = [
            {"role": "user", "content": "hello", "extra_field": "ignored"},
            {"role": "assistant", "content": None, "tool_calls": [{"id": "1"}]},
            {"role": "tool", "content": "result", "tool_call_id": "1", "name": "exec"},
        ]
        cleaned = provider._clean_messages(messages)
        assert len(cleaned) == 3
        # First message: only role + content
        assert "extra_field" not in cleaned[0]
        assert cleaned[0]["role"] == "user"
        assert cleaned[0]["content"] == "hello"
        # Second: content=None should be excluded
        assert "content" not in cleaned[1]
        assert cleaned[1]["tool_calls"] == [{"id": "1"}]
        # Third: tool message fields preserved
        assert cleaned[2]["tool_call_id"] == "1"
        assert cleaned[2]["name"] == "exec"


# ══════════════════════════════════════════════════════════════════════════════
# OpenRouterProvider
# ══════════════════════════════════════════════════════════════════════════════


class TestOpenRouterProvider:
    def _make_provider(self):
        with patch("echo_agent.models.providers.openai_provider.OpenAIProvider._build_client"):
            from echo_agent.models.providers.openrouter_provider import OpenRouterProvider
            provider = OpenRouterProvider(api_key="or-key", default_model="openrouter/auto")
        return provider

    def test_inherits_openai(self):
        from echo_agent.models.providers.openai_provider import OpenAIProvider
        from echo_agent.models.providers.openrouter_provider import OpenRouterProvider
        assert issubclass(OpenRouterProvider, OpenAIProvider)

    def test_base_url(self):
        provider = self._make_provider()
        assert provider.api_base == "https://openrouter.ai/api/v1"

    def test_build_params_with_provider_prefs(self):
        with patch("echo_agent.models.providers.openai_provider.OpenAIProvider._build_client"):
            from echo_agent.models.providers.openrouter_provider import OpenRouterProvider
            provider = OpenRouterProvider(
                api_key="or-key",
                default_model="model-x",
                provider_preferences={"order": ["anthropic"]},
            )
        messages = [{"role": "user", "content": "hi"}]
        params = provider._build_params(messages, None, None, None)
        assert "extra_body" in params
        assert params["extra_body"]["provider"] == {"order": ["anthropic"]}


# ══════════════════════════════════════════════════════════════════════════════
# AnthropicProvider
# ══════════════════════════════════════════════════════════════════════════════


class TestAnthropicProvider:
    def _make_provider(self, enable_cache=True):
        with patch("echo_agent.models.providers.anthropic_provider.AnthropicProvider._build_client"):
            from echo_agent.models.providers.anthropic_provider import AnthropicProvider
            provider = AnthropicProvider(
                api_key="ant-key",
                default_model="claude-sonnet-4-20250514",
                enable_cache=enable_cache,
            )
        return provider

    def test_build_params_caching_enabled(self):
        provider = self._make_provider(enable_cache=True)
        messages = [{"role": "user", "content": "hello"}]
        params = provider._build_params("claude-sonnet-4-20250514", messages, None, None)
        assert params["model"] == "claude-sonnet-4-20250514"
        assert "messages" in params
        assert "max_tokens" in params

    def test_build_params_caching_disabled(self):
        provider = self._make_provider(enable_cache=False)
        messages = [{"role": "user", "content": "hello"}]
        params = provider._build_params("claude-sonnet-4-20250514", messages, None, None)
        assert params["model"] == "claude-sonnet-4-20250514"

    def test_build_params_with_tools(self):
        provider = self._make_provider()
        messages = [{"role": "user", "content": "hello"}]
        tools = [{"type": "function", "function": {"name": "test", "parameters": {}}}]
        params = provider._build_params("claude-sonnet-4-20250514", messages, tools, None)
        assert "tools" in params


# ══════════════════════════════════════════════════════════════════════════════
# GeminiProvider
# ══════════════════════════════════════════════════════════════════════════════


class TestGeminiProvider:
    def test_get_default_model(self):
        with patch("echo_agent.models.providers.gemini_provider.GeminiProvider._build_client"):
            from echo_agent.models.providers.gemini_provider import GeminiProvider
            provider = GeminiProvider(api_key="gem-key", default_model="gemini-pro")
        assert provider.get_default_model() == "gemini-pro"

    def test_get_default_model_custom(self):
        with patch("echo_agent.models.providers.gemini_provider.GeminiProvider._build_client"):
            from echo_agent.models.providers.gemini_provider import GeminiProvider
            provider = GeminiProvider(api_key="gem-key", default_model="gemini-2.0-flash")
        assert provider.get_default_model() == "gemini-2.0-flash"


# ══════════════════════════════════════════════════════════════════════════════
# BedrockProvider
# ══════════════════════════════════════════════════════════════════════════════


class TestBedrockProvider:
    def test_is_claude_model_positive(self):
        from echo_agent.models.providers.bedrock_provider import _is_claude_model
        assert _is_claude_model("anthropic.claude-3-sonnet-20240229-v1:0") is True
        assert _is_claude_model("us.anthropic.claude-3-5-sonnet-20241022-v2:0") is True
        assert _is_claude_model("claude-v2") is True

    def test_is_claude_model_negative(self):
        from echo_agent.models.providers.bedrock_provider import _is_claude_model
        assert _is_claude_model("amazon.titan-text-express-v1") is False
        assert _is_claude_model("meta.llama3-70b-instruct-v1:0") is False
        assert _is_claude_model("cohere.command-r-plus-v1:0") is False
