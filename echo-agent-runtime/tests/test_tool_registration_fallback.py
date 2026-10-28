"""Tests for image_gen / tts tool registration fallback to main provider."""

from __future__ import annotations

import pytest

from echo_agent.agent.tools import (
    _is_openai_compatible_provider,
    _infer_image_model,
    _infer_tts_model,
    _try_register_image_gen,
    _try_register_tts,
)
from echo_agent.tools import Tool
from echo_agent.config.schema import Config, ImageGenConfig, ToolsConfig, TTSConfig
from echo_agent.models.provider import LLMProvider


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


class FakeOpenAIProvider(LLMProvider):
    """Simulates OpenAIProvider for testing (isinstance check needs real class)."""

    async def chat(self, messages, tools=None, model=None, tool_choice=None, **kw):
        pass  # pragma: no cover

    async def chat_stream(self, messages, tools=None, model=None, tool_choice=None, **kw):
        pass  # pragma: no cover

    def get_default_model(self) -> str:
        return ""


class FakeAnthropicProvider(LLMProvider):
    """Non-OpenAI provider."""

    async def chat(self, messages, tools=None, model=None, tool_choice=None, **kw):
        pass  # pragma: no cover

    async def chat_stream(self, messages, tools=None, model=None, tool_choice=None, **kw):
        pass  # pragma: no cover

    def get_default_model(self) -> str:
        return ""


def _make_config(image_gen_key="", image_gen_base="", image_gen_model="",
                 image_gen_backend="openai", image_gen_fal_key="", image_gen_fal_model="",
                 tts_key="", tts_base="", tts_model="", tts_backend="edge", tts_voice="") -> Config:
    """Build a minimal Config with tools section."""
    config = Config()
    config.tools = ToolsConfig()
    config.tools.image_gen = ImageGenConfig(
        backend=image_gen_backend, api_key=image_gen_key, api_base=image_gen_base, model=image_gen_model,
        fal_key=image_gen_fal_key, fal_model=image_gen_fal_model,
    )
    config.tools.tts = TTSConfig(openai_api_key=tts_key, openai_api_base=tts_base, model=tts_model, default_backend=tts_backend, default_voice=tts_voice)
    return config


# ---------------------------------------------------------------------------
# _is_openai_compatible_provider
# ---------------------------------------------------------------------------


class TestIsOpenAICompatibleProvider:

    def test_none_returns_false(self):
        assert _is_openai_compatible_provider(None) is False

    def test_anthropic_returns_false(self):
        provider = FakeAnthropicProvider(api_key="sk-ant-xxx")
        assert _is_openai_compatible_provider(provider) is False

    def test_openai_returns_true(self):
        from echo_agent.models.providers.openai_provider import OpenAIProvider
        provider = OpenAIProvider(api_key="sk-test", api_base="https://api.openai.com/v1")
        assert _is_openai_compatible_provider(provider) is True

    def test_openrouter_returns_true(self):
        from echo_agent.models.providers.openrouter_provider import OpenRouterProvider
        provider = OpenRouterProvider(api_key="sk-or-test")
        assert _is_openai_compatible_provider(provider) is True

    def test_wrapped_openai_returns_true(self):
        from echo_agent.models.providers.openai_provider import OpenAIProvider
        from echo_agent.models.rate_limiter import RateLimitedProvider, TokenBucketLimiter
        inner = OpenAIProvider(api_key="sk-test")
        wrapped = RateLimitedProvider(inner, TokenBucketLimiter(tokens_per_minute=60))
        assert _is_openai_compatible_provider(wrapped) is True

    def test_wrapped_anthropic_returns_false(self):
        pytest.importorskip("anthropic")
        from echo_agent.models.providers.anthropic_provider import AnthropicProvider
        from echo_agent.models.rate_limiter import RateLimitedProvider, TokenBucketLimiter
        inner = AnthropicProvider(api_key="sk-ant-test")
        wrapped = RateLimitedProvider(inner, TokenBucketLimiter(tokens_per_minute=60))
        assert _is_openai_compatible_provider(wrapped) is False


# ---------------------------------------------------------------------------
# _try_register_image_gen
# ---------------------------------------------------------------------------


class TestImageGenRegistration:

    def test_no_key_no_provider_skips(self):
        tools: list[Tool] = []
        config = _make_config()
        _try_register_image_gen(tools, config, provider=None)
        assert not any(t.name == "image_generate" for t in tools)

    def test_anthropic_provider_does_not_register(self):
        pytest.importorskip("anthropic")
        from echo_agent.models.providers.anthropic_provider import AnthropicProvider
        tools: list[Tool] = []
        config = _make_config()
        provider = AnthropicProvider(api_key="sk-ant-xxx")
        _try_register_image_gen(tools, config, provider=provider)
        assert not any(t.name == "image_generate" for t in tools)

    def test_openai_provider_without_explicit_config_does_not_register(self):
        from echo_agent.models.providers.openai_provider import OpenAIProvider
        tools: list[Tool] = []
        config = _make_config()
        provider = OpenAIProvider(api_key="sk-test", api_base="https://api.openai.com/v1")
        _try_register_image_gen(tools, config, provider=provider)
        assert not any(t.name == "image_generate" for t in tools)

    def test_minimax_provider_without_explicit_config_does_not_register(self):
        from echo_agent.models.providers.openai_provider import OpenAIProvider
        tools: list[Tool] = []
        config = _make_config()
        provider = OpenAIProvider(api_key="mm-key", api_base="https://api.minimax.chat/v1")
        _try_register_image_gen(tools, config, provider=provider)
        assert not any(t.name == "image_generate" for t in tools)

    def test_explicit_config_takes_priority(self):
        from echo_agent.models.providers.openai_provider import OpenAIProvider
        tools: list[Tool] = []
        config = _make_config(image_gen_key="my-key", image_gen_base="https://custom.api/v1", image_gen_model="my-model")
        provider = OpenAIProvider(api_key="provider-key", api_base="https://api.openai.com/v1")
        _try_register_image_gen(tools, config, provider=provider)
        img_tools = [t for t in tools if t.name == "image_generate"]
        assert len(img_tools) == 1
        assert img_tools[0]._api_key == "my-key"
        assert img_tools[0]._api_base == "https://custom.api/v1"
        assert img_tools[0]._model == "my-model"

    def test_pooled_provider_without_explicit_config_does_not_register(self):
        """credential_pool wrapped provider should NOT auto-register without explicit config."""
        from echo_agent.models.providers.openai_provider import OpenAIProvider
        from echo_agent.models.rate_limiter import RateLimitedProvider, TokenBucketLimiter
        from echo_agent.models.providers import _PooledProvider
        from echo_agent.models.credential_pool import CredentialPool
        from echo_agent.config.schema import ProviderConfig

        inner = OpenAIProvider(api_key="pool-key-1", api_base="https://api.minimax.chat/v1")
        pool = CredentialPool(["pool-key-1", "pool-key-2"])
        pc = ProviderConfig(name="openai", api_base="https://api.minimax.chat/v1")
        pooled = _PooledProvider(inner, pool, pc)
        wrapped = RateLimitedProvider(pooled, TokenBucketLimiter(tokens_per_minute=60))

        tools: list[Tool] = []
        config = _make_config()
        _try_register_image_gen(tools, config, provider=wrapped)
        assert not any(t.name == "image_generate" for t in tools)

    def test_fal_backend_no_key_skips(self):
        tools: list[Tool] = []
        config = _make_config(image_gen_backend="fal")
        _try_register_image_gen(tools, config, provider=None)
        assert not any(t.name == "image_generate" for t in tools)

    def test_fal_backend_with_key_registers(self):
        tools: list[Tool] = []
        config = _make_config(image_gen_backend="fal", image_gen_fal_key="fal-key-123", image_gen_fal_model="fal-ai/flux/schnell")
        _try_register_image_gen(tools, config, provider=None)
        img_tools = [t for t in tools if t.name == "image_generate"]
        assert len(img_tools) == 1
        assert img_tools[0]._fal_key == "fal-key-123"
        assert img_tools[0]._model == "fal-ai/flux/schnell"

    def test_fal_backend_default_model(self):
        tools: list[Tool] = []
        config = _make_config(image_gen_backend="fal", image_gen_fal_key="fal-key-123")
        _try_register_image_gen(tools, config, provider=None)
        img_tools = [t for t in tools if t.name == "image_generate"]
        assert len(img_tools) == 1
        assert img_tools[0]._model == "fal-ai/flux/schnell"


# ---------------------------------------------------------------------------
# _try_register_tts
# ---------------------------------------------------------------------------


class TestTTSRegistration:

    def test_default_backend_respected_without_key(self):
        from echo_agent.models.providers.openai_provider import OpenAIProvider
        tools: list[Tool] = []
        config = _make_config(tts_backend="edge")
        provider = OpenAIProvider(api_key="sk-test", api_base="https://api.openai.com/v1")
        _try_register_tts(tools, config, "/tmp", provider=provider)
        tts_tools = [t for t in tools if t.name == "text_to_speech"]
        assert len(tts_tools) == 1
        assert tts_tools[0]._default_backend == "edge"
        assert tts_tools[0]._openai_key == ""

    def test_anthropic_provider_does_not_inject_key(self):
        pytest.importorskip("anthropic")
        from echo_agent.models.providers.anthropic_provider import AnthropicProvider
        tools: list[Tool] = []
        config = _make_config(tts_backend="edge")
        provider = AnthropicProvider(api_key="sk-ant-xxx")
        _try_register_tts(tools, config, "/tmp", provider=provider)
        tts_tools = [t for t in tools if t.name == "text_to_speech"]
        assert len(tts_tools) == 1
        assert tts_tools[0]._openai_key == ""

    def test_openai_provider_without_explicit_key_does_not_inject(self):
        from echo_agent.models.providers.openai_provider import OpenAIProvider
        tools: list[Tool] = []
        config = _make_config(tts_backend="edge")
        provider = OpenAIProvider(api_key="sk-test", api_base="https://api.openai.com/v1")
        _try_register_tts(tools, config, "/tmp", provider=provider)
        tts_tools = [t for t in tools if t.name == "text_to_speech"]
        assert len(tts_tools) == 1
        assert tts_tools[0]._openai_key == ""
        assert tts_tools[0]._default_backend == "edge"

    def test_explicit_tts_key_takes_priority(self):
        from echo_agent.models.providers.openai_provider import OpenAIProvider
        tools: list[Tool] = []
        config = _make_config(tts_key="explicit-key", tts_base="https://api.openai.com/v1", tts_model="tts-1", tts_backend="openai")
        provider = OpenAIProvider(api_key="provider-key", api_base="https://api.openai.com/v1")
        _try_register_tts(tools, config, "/tmp", provider=provider)
        tts_tools = [t for t in tools if t.name == "text_to_speech"]
        assert len(tts_tools) == 1
        assert tts_tools[0]._openai_key == "explicit-key"
        assert tts_tools[0]._default_backend == "openai"


# ---------------------------------------------------------------------------
# Model inference helpers
# ---------------------------------------------------------------------------


class TestModelInference:

    @pytest.mark.parametrize("base,expected", [
        ("https://api.minimax.chat/v1", "image-01"),
        ("https://dashscope.aliyuncs.com/v1", "wanx-v1"),
        ("https://open.bigmodel.cn/api/v1", "cogview-3"),
        ("https://api.openai.com/v1", "dall-e-3"),
        ("", "dall-e-3"),
    ])
    def test_infer_image_model(self, base, expected):
        assert _infer_image_model(base) == expected

    @pytest.mark.parametrize("base,expected", [
        ("https://api.minimax.chat/v1", "speech-02"),
        ("https://dashscope.aliyuncs.com/v1", "cosyvoice-v1"),
        ("https://api.openai.com/v1", "tts-1"),
        ("", "tts-1"),
    ])
    def test_infer_tts_model(self, base, expected):
        assert _infer_tts_model(base) == expected
