"""Tests for model providers — StubProvider, TokenCounter, OpenAI, OpenRouter, Anthropic, Gemini, Bedrock."""

from __future__ import annotations

from unittest.mock import AsyncMock, MagicMock, patch

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


# ══════════════════════════════════════════════════════════════════════════════
# OpenAIProvider — reasoning_content 提升
# ══════════════════════════════════════════════════════════════════════════════


def _fake_openai_resp(content, reasoning=None, finish_reason="stop", tool_calls=None):
    """Build a minimal object mimicking openai SDK chat.completion response."""
    msg = MagicMock()
    msg.content = content
    msg.reasoning_content = reasoning
    msg.tool_calls = tool_calls
    choice = MagicMock()
    choice.message = msg
    choice.finish_reason = finish_reason
    resp = MagicMock()
    resp.choices = [choice]
    resp.usage = None
    resp.model = "gpt-5.5"
    return resp


class TestOpenAIReasoningPromotion:
    def _provider(self):
        from echo_agent.models.providers.openai_provider import OpenAIProvider
        with patch.object(OpenAIProvider, "_build_client", return_value=MagicMock()):
            return OpenAIProvider(api_key="x", default_model="gpt-5.5")

    def test_promote_when_content_empty(self):
        provider = self._provider()
        resp = provider._parse_response(_fake_openai_resp(content="", reasoning="real answer"))
        assert resp.content == "real answer"
        # Promotion moves the text: the reasoning slot must be cleared so the
        # same text is not emitted twice (thinking event + answer body).
        assert resp.reasoning_content is None

    def test_no_promote_when_content_present(self):
        provider = self._provider()
        resp = provider._parse_response(_fake_openai_resp(content="hi", reasoning="thinking..."))
        assert resp.content == "hi"
        assert resp.reasoning_content == "thinking..."

    def test_promote_when_finish_length(self):
        # A reasoning model that burned its whole budget on reasoning leaves
        # content empty with finish_reason="length"; the truncated reasoning
        # is the only recoverable answer material and must be promoted.
        provider = self._provider()
        resp = provider._parse_response(
            _fake_openai_resp(content="", reasoning="partial", finish_reason="length")
        )
        assert resp.content == "partial"
        assert resp.reasoning_content is None

    def test_no_promote_when_finish_error(self):
        provider = self._provider()
        resp = provider._parse_response(
            _fake_openai_resp(content="", reasoning="partial", finish_reason="error")
        )
        assert resp.content == "" or resp.content is None

    def test_reasoning_field_missing_no_error(self):
        provider = self._provider()
        msg = MagicMock(spec=["content", "tool_calls"])
        msg.content = "ok"
        msg.tool_calls = None
        choice = MagicMock()
        choice.message = msg
        choice.finish_reason = "stop"
        resp_obj = MagicMock()
        resp_obj.choices = [choice]
        resp_obj.usage = None
        resp_obj.model = "gpt-5.5"
        resp = provider._parse_response(resp_obj)
        assert resp.content == "ok"
        assert resp.reasoning_content is None


class _FakeStreamChunk:
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


class _FakeStream:
    def __init__(self, chunks):
        self._chunks = chunks

    def __aiter__(self):
        async def gen():
            for c in self._chunks:
                yield c
        return gen()


class TestOpenAIStreamReasoning:
    def _provider(self):
        from echo_agent.models.providers.openai_provider import OpenAIProvider
        with patch.object(OpenAIProvider, "_build_client", return_value=MagicMock()):
            return OpenAIProvider(api_key="x", default_model="gpt-5.5")

    @pytest.mark.asyncio
    async def test_stream_promotes_reasoning_when_content_empty(self):
        provider = self._provider()
        chunks = [
            _FakeStreamChunk(reasoning="real "),
            _FakeStreamChunk(reasoning="answer"),
            _FakeStreamChunk(finish_reason="stop"),
        ]
        provider._client.chat.completions.create = AsyncMock(return_value=_FakeStream(chunks))
        resp = await provider.chat_stream(messages=[{"role": "user", "content": "hi"}])
        assert resp.content == "real answer"
        # Promotion moves the text — reasoning is cleared to prevent the same
        # text being shown twice (thinking event + answer body).
        assert resp.reasoning_content is None

    @pytest.mark.asyncio
    async def test_stream_keeps_content_when_present(self):
        provider = self._provider()
        chunks = [
            _FakeStreamChunk(content="hi", reasoning="think"),
            _FakeStreamChunk(finish_reason="stop"),
        ]
        provider._client.chat.completions.create = AsyncMock(return_value=_FakeStream(chunks))
        resp = await provider.chat_stream(messages=[{"role": "user", "content": "hi"}])
        assert resp.content == "hi"


# ══════════════════════════════════════════════════════════════════════════════
# LLMProvider.aclose — release SDK clients in the loop that owns their sockets
# ══════════════════════════════════════════════════════════════════════════════


class TestProviderAclose:
    """aclose() exists so short-lived providers (setup's model verification)
    release their httpx.AsyncClient before asyncio.run tears the loop down.
    Leaving it open lets the SDK's __del__ schedule aclose() on a later,
    unrelated loop, which raises "Event loop is closed" from an unawaited task.
    """

    def _provider(self, client):
        prov = StubProvider(message="x")
        prov._client = client
        return prov

    @pytest.mark.asyncio
    async def test_awaits_async_close(self):
        client = MagicMock()
        client.close = AsyncMock()
        await self._provider(client).aclose()
        client.close.assert_awaited_once()

    @pytest.mark.asyncio
    async def test_falls_back_to_aclose_name(self):
        client = MagicMock(spec=["aclose"])
        client.aclose = AsyncMock()
        await self._provider(client).aclose()
        client.aclose.assert_awaited_once()

    @pytest.mark.asyncio
    async def test_sync_close_is_called_without_await(self):
        client = MagicMock(spec=["close"])
        client.close = MagicMock(return_value=None)
        await self._provider(client).aclose()
        client.close.assert_called_once()

    @pytest.mark.asyncio
    async def test_no_client_is_noop(self):
        # Gemini holds a module handle, Bedrock builds clients per call: neither
        # has a closeable _client, and aclose must stay quiet rather than raise.
        await StubProvider(message="x").aclose()

    @pytest.mark.asyncio
    async def test_client_without_close_is_noop(self):
        await self._provider(MagicMock(spec=[])).aclose()

    @pytest.mark.asyncio
    async def test_close_error_is_swallowed(self):
        client = MagicMock(spec=["close"])
        client.close = AsyncMock(side_effect=RuntimeError("Event loop is closed"))
        await self._provider(client).aclose()  # must not raise

    @pytest.mark.asyncio
    async def test_second_close_is_safe(self):
        client = MagicMock()
        client.close = AsyncMock()
        prov = self._provider(client)
        await prov.aclose()
        await prov.aclose()
        assert client.close.await_count == 2
