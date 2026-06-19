"""Contract tests for the provider factory (__init__.py), format_utils, and
remaining gaps in openai/anthropic providers.

SDKs are mocked; no network access.
"""

from __future__ import annotations

from unittest.mock import MagicMock, patch

import pytest

from echo_agent.config.schema import ProviderConfig
from echo_agent.models.provider import LLMProvider, LLMResponse
from echo_agent.models.providers import (
    create_provider,
    validate_provider_config,
)
from echo_agent.models.providers.format_utils import (
    _map_stop_reason,
    _sanitize_tool_id,
    anthropic_response_to_llm_fields,
    openai_to_anthropic_messages,
    openai_to_anthropic_tools,
)


# ══════════════════════════════════════════════════════════════════════════════
# validate_provider_config
# ══════════════════════════════════════════════════════════════════════════════


class TestValidateProviderConfig:
    def test_empty_name_raises(self):
        with pytest.raises(ValueError, match="provider name is required"):
            validate_provider_config(ProviderConfig(name="", models=["m"]))

    def test_unknown_provider_requires_api_base(self):
        with pytest.raises(ValueError, match="requires api_base"):
            validate_provider_config(ProviderConfig(name="custom", models=["m"]))

    def test_missing_model_raises(self):
        with pytest.raises(ValueError, match="requires an explicit model"):
            validate_provider_config(ProviderConfig(name="openai", api_key="k"))

    def test_bedrock_without_credentials_ok(self, monkeypatch):
        for var in ("AWS_ACCESS_KEY_ID", "AWS_PROFILE", "AWS_WEB_IDENTITY_TOKEN_FILE"):
            monkeypatch.delenv(var, raising=False)
        # Bedrock may resolve role credentials at call time — no error expected.
        validate_provider_config(ProviderConfig(name="bedrock", models=["anthropic.claude-3"]))

    def test_bedrock_with_env_credentials_ok(self, monkeypatch):
        monkeypatch.setenv("AWS_ACCESS_KEY_ID", "AKIA")
        validate_provider_config(ProviderConfig(name="aws", models=["anthropic.claude-3"]))

    def test_api_key_satisfies(self):
        validate_provider_config(ProviderConfig(name="openai", api_key="k", models=["m"]))

    def test_env_api_key_satisfies(self, monkeypatch):
        monkeypatch.setenv("OPENAI_API_KEY", "from-env")
        validate_provider_config(ProviderConfig(name="openai", models=["m"]))

    def test_credential_pool_satisfies(self):
        validate_provider_config(
            ProviderConfig(name="openai", credential_pool=["a", "b"], models=["m"])
        )

    def test_keyless_openai_compatible_with_base(self, monkeypatch):
        monkeypatch.delenv("OPENAI_API_KEY", raising=False)
        # OpenAI with api_base is allowed keyless (local servers, etc.)
        validate_provider_config(
            ProviderConfig(name="openai", api_base="http://localhost:8000", models=["m"])
        )

    def test_missing_api_key_raises_with_hint(self, monkeypatch):
        monkeypatch.delenv("ANTHROPIC_API_KEY", raising=False)
        with pytest.raises(ValueError, match="ANTHROPIC_API_KEY"):
            validate_provider_config(ProviderConfig(name="anthropic", models=["m"]))


# ══════════════════════════════════════════════════════════════════════════════
# create_provider
# ══════════════════════════════════════════════════════════════════════════════


class TestCreateProvider:
    def test_creates_openai(self):
        with patch("echo_agent.models.providers.openai_provider.OpenAIProvider._build_client"):
            provider = create_provider(
                ProviderConfig(name="openai", api_key="k", models=["gpt-4"])
            )
        from echo_agent.models.providers.openai_provider import OpenAIProvider

        assert isinstance(provider, OpenAIProvider)
        assert provider.get_default_model() == "gpt-4"

    def test_default_model_override_takes_precedence(self):
        with patch("echo_agent.models.providers.openai_provider.OpenAIProvider._build_client"):
            provider = create_provider(
                ProviderConfig(name="openai", api_key="k", models=["gpt-4"]),
                default_model="gpt-4o",
            )
        assert provider.get_default_model() == "gpt-4o"

    def test_unknown_provider_falls_back_to_openai(self):
        with patch("echo_agent.models.providers.openai_provider.OpenAIProvider._build_client"):
            provider = create_provider(
                ProviderConfig(name="mystery", api_base="http://x", models=["m"])
            )
        from echo_agent.models.providers.openai_provider import OpenAIProvider

        assert isinstance(provider, OpenAIProvider)

    def test_timeout_wired_from_config(self):
        with patch("echo_agent.models.providers.openai_provider.OpenAIProvider._build_client"):
            provider = create_provider(
                ProviderConfig(name="openai", api_key="k", models=["m"], timeout_seconds=42)
            )
        assert provider.request_timeout == 42.0

    def test_extra_headers_passed(self):
        with patch("echo_agent.models.providers.openai_provider.OpenAIProvider._build_client"):
            provider = create_provider(
                ProviderConfig(
                    name="openai", api_key="k", models=["m"], extra_headers={"X-A": "1"}
                )
            )
        assert provider._extra_headers == {"X-A": "1"}

    def test_rate_limit_wraps_provider(self):
        from echo_agent.models.rate_limiter import RateLimitedProvider

        with patch("echo_agent.models.providers.openai_provider.OpenAIProvider._build_client"):
            provider = create_provider(
                ProviderConfig(name="openai", api_key="k", models=["m"], rate_limit_rpm=60)
            )
        assert isinstance(provider, RateLimitedProvider)

    def test_credential_pool_wraps_provider(self):
        with patch("echo_agent.models.providers.openai_provider.OpenAIProvider._build_client"):
            provider = create_provider(
                ProviderConfig(name="openai", credential_pool=["k1", "k2"], models=["m"])
            )
        # Pooled provider delegates get_default_model.
        assert provider.get_default_model() == "m"


# ══════════════════════════════════════════════════════════════════════════════
# _PooledProvider — credential rotation on error
# ══════════════════════════════════════════════════════════════════════════════


class _FakeInner(LLMProvider):
    def __init__(self):
        super().__init__(api_key="k1")
        self.calls = 0
        self.responses: list[LLMResponse] = []
        self._client = MagicMock()

    def _build_client(self):
        return MagicMock()

    async def chat(self, messages, tools=None, model=None, tool_choice=None, **kwargs):
        resp = self.responses[min(self.calls, len(self.responses) - 1)]
        self.calls += 1
        return resp

    async def chat_stream(self, messages, tools=None, model=None, tool_choice=None, on_delta=None, **kwargs):
        resp = self.responses[min(self.calls, len(self.responses) - 1)]
        self.calls += 1
        return resp

    def get_default_model(self):
        return "m"


class TestPooledProvider:
    def _make(self, inner):
        from echo_agent.models.credential_pool import CredentialPool
        from echo_agent.models.providers import _PooledProvider

        pool = CredentialPool(["k1", "k2"])
        cfg = ProviderConfig(name="openai", credential_pool=["k1", "k2"], models=["m"])
        return _PooledProvider(inner, pool, type(inner), cfg)

    @pytest.mark.asyncio
    async def test_rotates_on_error(self):
        inner = _FakeInner()
        inner.responses = [
            LLMResponse(content="Error: rate limit", finish_reason="error"),
            LLMResponse(content="recovered", finish_reason="stop"),
        ]
        pooled = self._make(inner)
        resp = await pooled.chat(messages=[{"role": "user", "content": "hi"}])
        assert resp.content == "recovered"
        assert inner.calls == 2

    @pytest.mark.asyncio
    async def test_success_no_rotation(self):
        inner = _FakeInner()
        inner.responses = [LLMResponse(content="ok", finish_reason="stop")]
        pooled = self._make(inner)
        resp = await pooled.chat(messages=[{"role": "user", "content": "hi"}])
        assert resp.content == "ok"
        assert inner.calls == 1

    @pytest.mark.asyncio
    async def test_get_default_model_delegates(self):
        inner = _FakeInner()
        pooled = self._make(inner)
        assert pooled.get_default_model() == "m"

    @pytest.mark.asyncio
    async def test_stream_rotates_on_error_when_nothing_emitted(self):
        inner = _FakeInner()
        inner.responses = [
            LLMResponse(content="Error: rate limit", finish_reason="error"),
            LLMResponse(content="recovered", finish_reason="stop"),
        ]
        pooled = self._make(inner)
        deltas: list[str] = []

        async def on_delta(d):
            deltas.append(d)

        resp = await pooled.chat_stream(
            messages=[{"role": "user", "content": "hi"}], on_delta=on_delta
        )
        assert resp.content == "recovered"
        assert inner.calls == 2

    @pytest.mark.asyncio
    async def test_stream_success_no_rotation(self):
        inner = _FakeInner()
        inner.responses = [LLMResponse(content="ok", finish_reason="stop")]
        pooled = self._make(inner)
        resp = await pooled.chat_stream(messages=[{"role": "user", "content": "hi"}])
        assert resp.content == "ok"
        assert inner.calls == 1


# ══════════════════════════════════════════════════════════════════════════════
# format_utils — openai_to_anthropic_messages
# ══════════════════════════════════════════════════════════════════════════════


class TestOpenaiToAnthropicMessages:
    def test_system_string(self):
        system, _ = openai_to_anthropic_messages([{"role": "system", "content": "sys"}])
        assert system == [{"type": "text", "text": "sys"}]

    def test_system_empty_placeholder(self):
        system, _ = openai_to_anthropic_messages([{"role": "system", "content": ""}])
        assert system[0]["text"] == "(empty)"

    def test_system_block_list(self):
        system, _ = openai_to_anthropic_messages(
            [{"role": "system", "content": [{"type": "text", "text": "a"}]}]
        )
        assert system[0]["text"] == "a"

    def test_user_message_wrapped_in_blocks(self):
        _, converted = openai_to_anthropic_messages([{"role": "user", "content": "hi"}])
        assert converted == [{"role": "user", "content": [{"type": "text", "text": "hi"}]}]

    def test_assistant_with_tool_calls(self):
        _, converted = openai_to_anthropic_messages([
            {
                "role": "assistant",
                "content": "calling",
                "tool_calls": [
                    {"id": "abc", "function": {"name": "exec", "arguments": '{"x": 1}'}}
                ],
            }
        ])
        blocks = converted[0]["content"]
        tool_use = [b for b in blocks if b["type"] == "tool_use"][0]
        assert tool_use["name"] == "exec"
        assert tool_use["input"] == {"x": 1}

    def test_assistant_empty_gets_placeholder(self):
        _, converted = openai_to_anthropic_messages([{"role": "assistant", "content": ""}])
        assert converted[0]["content"] == [{"type": "text", "text": "(empty)"}]

    def test_tool_result_merged_into_user(self):
        _, converted = openai_to_anthropic_messages([
            {"role": "user", "content": "q"},
            {"role": "tool", "tool_call_id": "abc", "content": "result"},
        ])
        # tool result appended to preceding user message
        last_user = converted[0]
        assert last_user["role"] == "user"
        tool_results = [b for b in last_user["content"] if b.get("type") == "tool_result"]
        assert tool_results[0]["content"] == "result"

    def test_alternation_merges_consecutive_same_role(self):
        _, converted = openai_to_anthropic_messages([
            {"role": "user", "content": "a"},
            {"role": "user", "content": "b"},
        ])
        assert len(converted) == 1
        texts = [blk["text"] for blk in converted[0]["content"]]
        assert texts == ["a", "b"]

    def test_cache_markers_injected(self):
        system, converted = openai_to_anthropic_messages(
            [
                {"role": "system", "content": "sys"},
                {"role": "user", "content": "hi"},
            ],
            inject_cache_markers=True,
        )
        assert system[-1]["cache_control"] == {"type": "ephemeral"}
        assert converted[-1]["content"][-1]["cache_control"] == {"type": "ephemeral"}

    def test_image_url_base64_block(self):
        _, converted = openai_to_anthropic_messages([
            {
                "role": "user",
                "content": [
                    {"type": "image_url", "image_url": {"url": "data:image/png;base64,Zm9v"}}
                ],
            }
        ])
        img = converted[0]["content"][0]
        assert img["type"] == "image"
        assert img["source"]["type"] == "base64"
        assert img["source"]["media_type"] == "image/png"
        assert img["source"]["data"] == "Zm9v"

    def test_image_url_remote(self):
        _, converted = openai_to_anthropic_messages([
            {
                "role": "user",
                "content": [
                    {"type": "image_url", "image_url": {"url": "https://x/y.png"}}
                ],
            }
        ])
        img = converted[0]["content"][0]
        assert img["source"]["type"] == "url"
        assert img["source"]["url"] == "https://x/y.png"


# ══════════════════════════════════════════════════════════════════════════════
# format_utils — helpers
# ══════════════════════════════════════════════════════════════════════════════


class TestFormatUtilsHelpers:
    def test_sanitize_tool_id_strips_invalid(self):
        assert _sanitize_tool_id("abc-123_X") == "abc-123_X"
        assert _sanitize_tool_id("a b!c") == "a_b_c"

    def test_sanitize_tool_id_empty_default(self):
        assert _sanitize_tool_id("") == "tool_0"
        assert _sanitize_tool_id("!!!") == "___"

    def test_map_stop_reason_known(self):
        assert _map_stop_reason("end_turn") == "stop"
        assert _map_stop_reason("tool_use") == "tool_calls"
        assert _map_stop_reason("max_tokens") == "length"
        assert _map_stop_reason("refusal") == "content_filter"

    def test_map_stop_reason_unknown_passthrough(self):
        assert _map_stop_reason("weird") == "weird"
        assert _map_stop_reason("") == "stop"

    def test_tools_conversion(self):
        out = openai_to_anthropic_tools([
            {"function": {"name": "fn", "description": "d", "parameters": {"type": "object"}}}
        ])
        assert out[0]["name"] == "fn"
        assert out[0]["input_schema"] == {"type": "object"}

    def test_tools_default_schema(self):
        out = openai_to_anthropic_tools([{"name": "bare"}])
        assert out[0]["input_schema"] == {"type": "object", "properties": {}}

    def test_tools_cache_marker(self):
        out = openai_to_anthropic_tools(
            [{"function": {"name": "fn"}}], inject_cache_markers=True
        )
        assert out[-1]["cache_control"] == {"type": "ephemeral"}


# ══════════════════════════════════════════════════════════════════════════════
# format_utils — anthropic_response_to_llm_fields
# ══════════════════════════════════════════════════════════════════════════════


class TestAnthropicResponseToLlmFields:
    def test_text_blocks(self):
        fields = anthropic_response_to_llm_fields(
            [{"type": "text", "text": "a"}, {"type": "text", "text": "b"}],
            stop_reason="end_turn",
            usage={"input_tokens": 3, "output_tokens": 4},
            model="claude",
        )
        assert fields["content"] == "a\nb"
        assert fields["finish_reason"] == "stop"
        assert fields["usage"] == {"prompt_tokens": 3, "completion_tokens": 4}

    def test_tool_use_block(self):
        fields = anthropic_response_to_llm_fields(
            [{"type": "tool_use", "id": "t1", "name": "exec", "input": {"cmd": "ls"}}],
            stop_reason="tool_use",
        )
        assert fields["content"] is None
        assert fields["finish_reason"] == "tool_calls"
        tc = fields["tool_calls"][0]
        assert tc.id == "t1"
        assert tc.name == "exec"

    def test_cached_tokens_summed(self):
        fields = anthropic_response_to_llm_fields(
            [{"type": "text", "text": "x"}],
            usage={
                "input_tokens": 1,
                "output_tokens": 2,
                "cache_read_input_tokens": 10,
                "cache_creation_input_tokens": 5,
            },
        )
        assert fields["usage"]["cached_tokens"] == 15
