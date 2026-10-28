"""Contract tests filling gaps in OpenAIProvider and AnthropicProvider.

SDKs mocked; no network access. Focus on chat() happy paths, response parsing,
embeddings, streaming tool-call assembly, and Anthropic thinking/param building.
"""

from __future__ import annotations

from unittest.mock import AsyncMock, MagicMock, patch

import pytest



# ══════════════════════════════════════════════════════════════════════════════
# Helpers
# ══════════════════════════════════════════════════════════════════════════════


def _openai_provider(default_model="gpt-4", **kw):
    from echo_agent.models.providers.openai_provider import OpenAIProvider

    with patch.object(OpenAIProvider, "_build_client", return_value=MagicMock()):
        return OpenAIProvider(api_key="k", default_model=default_model, **kw)


def _anthropic_provider(default_model="claude-sonnet-4-20250514", **kw):
    from echo_agent.models.providers.anthropic_provider import AnthropicProvider

    with patch.object(AnthropicProvider, "_build_client", return_value=MagicMock()):
        return AnthropicProvider(api_key="k", default_model=default_model, **kw)


# ══════════════════════════════════════════════════════════════════════════════
# OpenAI — chat / embed / parse
# ══════════════════════════════════════════════════════════════════════════════


class TestOpenAIChat:
    @pytest.mark.asyncio
    async def test_chat_happy_path(self):
        provider = _openai_provider()
        msg = MagicMock()
        msg.content = "hello"
        msg.reasoning_content = None
        msg.tool_calls = None
        choice = MagicMock()
        choice.message = msg
        choice.finish_reason = "stop"
        resp = MagicMock()
        resp.choices = [choice]
        resp.usage = MagicMock(prompt_tokens=5, completion_tokens=3)
        resp.model = "gpt-4"
        provider._client.chat.completions.create = AsyncMock(return_value=resp)

        out = await provider.chat(messages=[{"role": "user", "content": "hi"}])
        assert out.content == "hello"
        assert out.usage == {"prompt_tokens": 5, "completion_tokens": 3}

    @pytest.mark.asyncio
    async def test_chat_error_mapping(self):
        provider = _openai_provider()
        provider._client.chat.completions.create = AsyncMock(side_effect=RuntimeError("api down"))
        out = await provider.chat(messages=[{"role": "user", "content": "hi"}])
        assert out.finish_reason == "error"
        assert "api down" in out.content

    @pytest.mark.asyncio
    async def test_embed_success(self):
        provider = _openai_provider()
        item = MagicMock()
        item.embedding = [0.1, 0.2, 0.3]
        resp = MagicMock()
        resp.data = [item]
        provider._client.embeddings.create = AsyncMock(return_value=resp)
        vec = await provider.embed("text")
        assert vec == [0.1, 0.2, 0.3]

    @pytest.mark.asyncio
    async def test_embed_error_returns_none(self):
        provider = _openai_provider()
        provider._client.embeddings.create = AsyncMock(side_effect=RuntimeError("x"))
        assert await provider.embed("text") is None

    def test_clean_messages_drops_anthropic_thinking_blocks(self):
        """Assistant messages carry thinking_blocks for the Anthropic converter;
        forwarding that key to an OpenAI-shaped API would be rejected."""
        provider = _openai_provider()
        cleaned = provider._clean_messages([
            {"role": "assistant", "content": "hi", "tool_calls": [],
             "thinking_blocks": [{"type": "thinking", "thinking": "p",
                                  "signature": "SIG"}]},
        ])
        assert cleaned == [{"role": "assistant", "content": "hi", "tool_calls": []}]

    def test_parse_response_no_choices(self):
        provider = _openai_provider()
        resp = MagicMock()
        resp.choices = []
        out = provider._parse_response(resp)
        assert out.finish_reason == "error"

    @pytest.mark.asyncio
    async def test_non_json_200_does_not_leak_attributeerror(self):
        # The exact reported failure: apiBase without /v1 hits the gateway index,
        # which answers 200 + text/html. With _strict_response_validation off the
        # SDK returns response.text — a plain str — and reaching for .choices on
        # it used to raise "'str' object has no attribute 'choices'" out of
        # chat(), destroying the response body along the way.
        provider = _openai_provider()
        provider._client.chat.completions.create = AsyncMock(
            return_value="<html>gateway</html>"
        )
        out = await provider.chat(messages=[{"role": "user", "content": "hi"}])
        assert out.finish_reason == "error"
        assert "choices" not in out.content
        assert "apiBase" in out.content
        assert "/v1" in out.content
        # The body that actually came back must survive for diagnosis.
        assert "gateway" in out.content

    @pytest.mark.asyncio
    async def test_non_json_200_is_not_retried(self):
        # An endpoint answering in the wrong shape will answer the same way
        # again, so the failure must classify as permanent rather than burn
        # three attempts and 6s of backoff on a fixed misconfiguration.
        provider = _openai_provider()
        create = AsyncMock(return_value="<html>connection timeout</html>")
        provider._client.chat.completions.create = create
        out = await provider.chat_with_retry(messages=[{"role": "user", "content": "hi"}])
        assert out.finish_reason == "error"
        assert create.await_count == 1

    @pytest.mark.asyncio
    async def test_unparseable_stream_reports_cause(self):
        # Same hazard on the streaming path: `async for` over a str reports
        # "not async iterable", which names the symptom and not the cause.
        provider = _openai_provider()
        provider._client.chat.completions.create = AsyncMock(
            return_value="<html>gateway</html>"
        )
        out = await provider.chat_stream(messages=[{"role": "user", "content": "hi"}])
        assert out.finish_reason == "error"
        assert "apiBase" in out.content

    def test_parse_response_rejects_object_without_choices(self):
        provider = _openai_provider()
        out = provider._parse_response(object())
        assert out.finish_reason == "error"
        assert "choices" in out.content  # names the missing field
        assert "apiBase" in out.content

    def test_parse_response_tool_calls(self):
        provider = _openai_provider()
        tc = MagicMock()
        tc.id = "call_1"
        tc.function.name = "exec"
        tc.function.arguments = '{"cmd": "ls"}'
        msg = MagicMock()
        msg.content = None
        msg.reasoning_content = None
        msg.tool_calls = [tc]
        choice = MagicMock()
        choice.message = msg
        choice.finish_reason = "tool_calls"
        resp = MagicMock()
        resp.choices = [choice]
        resp.usage = None
        resp.model = "gpt-4"
        out = provider._parse_response(resp)
        assert out.tool_calls[0].name == "exec"
        assert out.tool_calls[0].arguments == {"cmd": "ls"}

    def test_parse_response_bad_tool_args_keeps_raw(self):
        provider = _openai_provider()
        tc = MagicMock()
        tc.id = "call_1"
        tc.function.name = "exec"
        tc.function.arguments = "not-json"
        msg = MagicMock()
        msg.content = None
        msg.reasoning_content = None
        msg.tool_calls = [tc]
        choice = MagicMock()
        choice.message = msg
        choice.finish_reason = "tool_calls"
        resp = MagicMock()
        resp.choices = [choice]
        resp.usage = None
        resp.model = "gpt-4"
        out = provider._parse_response(resp)
        assert out.tool_calls[0].arguments == {"raw": "not-json"}


# ══════════════════════════════════════════════════════════════════════════════
# OpenAI — streaming tool-call assembly + fallback
# ══════════════════════════════════════════════════════════════════════════════


class _ToolDelta:
    def __init__(self, index, tc_id=None, name=None, args=None):
        self.index = index
        self.id = tc_id
        fn = MagicMock(spec=["name", "arguments"])
        fn.name = name
        fn.arguments = args
        self.function = fn


class _StreamChunk:
    def __init__(self, content=None, tool_calls=None, finish_reason=None):
        delta = MagicMock(spec=["content", "reasoning_content", "tool_calls"])
        delta.content = content
        delta.reasoning_content = None
        delta.tool_calls = tool_calls
        choice = MagicMock()
        choice.delta = delta
        choice.finish_reason = finish_reason
        self.choices = [choice]
        self.model = "gpt-4"
        self.usage = None


class _AsyncStream:
    def __init__(self, chunks):
        self._chunks = chunks

    def __aiter__(self):
        async def gen():
            for c in self._chunks:
                yield c

        return gen()


class TestOpenAIStreamToolCalls:
    @pytest.mark.asyncio
    async def test_stream_assembles_tool_call(self):
        provider = _openai_provider()
        chunks = [
            _StreamChunk(tool_calls=[_ToolDelta(0, tc_id="call_1", name="exec", args='{"cmd":')]),
            _StreamChunk(tool_calls=[_ToolDelta(0, args=' "ls"}')]),
            _StreamChunk(finish_reason="tool_calls"),
        ]
        provider._client.chat.completions.create = AsyncMock(return_value=_AsyncStream(chunks))
        out = await provider.chat_stream(messages=[{"role": "user", "content": "hi"}])
        assert out.finish_reason == "tool_calls"
        assert out.tool_calls[0].name == "exec"
        assert out.tool_calls[0].arguments == {"cmd": "ls"}

    @pytest.mark.asyncio
    async def test_stream_emits_content_deltas(self):
        provider = _openai_provider()
        chunks = [
            _StreamChunk(content="he"),
            _StreamChunk(content="llo"),
            _StreamChunk(finish_reason="stop"),
        ]
        provider._client.chat.completions.create = AsyncMock(return_value=_AsyncStream(chunks))
        deltas: list[str] = []

        async def on_delta(d):
            deltas.append(d)

        out = await provider.chat_stream(
            messages=[{"role": "user", "content": "hi"}], on_delta=on_delta
        )
        assert out.content == "hello"
        assert deltas == ["he", "llo"]

    @pytest.mark.asyncio
    async def test_stream_init_failure_falls_back_to_chat(self):
        provider = _openai_provider()
        # Stream init raises, the stream retry (without stream_options) also
        # raises, so we finally fall back to the non-stream chat which succeeds.
        msg = MagicMock()
        msg.content = "fallback"
        msg.reasoning_content = None
        msg.tool_calls = None
        choice = MagicMock()
        choice.message = msg
        choice.finish_reason = "stop"
        resp = MagicMock()
        resp.choices = [choice]
        resp.usage = None
        resp.model = "gpt-4"
        provider._client.chat.completions.create = AsyncMock(
            side_effect=[RuntimeError("stream boom"), RuntimeError("retry boom"), resp]
        )
        out = await provider.chat_stream(messages=[{"role": "user", "content": "hi"}])
        assert out.content == "fallback"

    @pytest.mark.asyncio
    async def test_stream_init_failure_retries_without_stream_options(self):
        # An endpoint that rejects stream_options should still stream: the first
        # attempt (with the field) fails, and the retry (without it) succeeds,
        # preserving token-by-token output — we only lose usage.
        provider = _openai_provider()
        chunks = [_StreamChunk(content="hi"), _StreamChunk(finish_reason="stop")]
        create = AsyncMock(side_effect=[RuntimeError("bad stream_options"), _AsyncStream(chunks)])
        provider._client.chat.completions.create = create
        out = await provider.chat_stream(messages=[{"role": "user", "content": "hi"}])
        assert out.content == "hi"
        assert create.await_count == 2
        assert create.await_args_list[0].kwargs.get("stream_options") == {"include_usage": True}
        assert "stream_options" not in create.await_args_list[1].kwargs

    @pytest.mark.asyncio
    async def test_stream_include_usage_false_omits_field(self):
        # A provider configured with stream_include_usage=False must never send
        # stream_options (for endpoints that reject it outright).
        provider = _openai_provider(stream_include_usage=False)
        chunks = [_StreamChunk(content="hi"), _StreamChunk(finish_reason="stop")]
        create = AsyncMock(return_value=_AsyncStream(chunks))
        provider._client.chat.completions.create = create
        await provider.chat_stream(messages=[{"role": "user", "content": "hi"}])
        assert "stream_options" not in create.call_args.kwargs

    @pytest.mark.asyncio
    async def test_caller_can_override_stream_options(self):
        # A caller passing stream_options explicitly must have it honored (not
        # silently dropped by _build_params).
        provider = _openai_provider()
        chunks = [_StreamChunk(content="hi"), _StreamChunk(finish_reason="stop")]
        create = AsyncMock(return_value=_AsyncStream(chunks))
        provider._client.chat.completions.create = create
        await provider.chat_stream(
            messages=[{"role": "user", "content": "hi"}],
            stream_options={"include_usage": False},
        )
        assert create.call_args.kwargs["stream_options"] == {"include_usage": False}

    @pytest.mark.asyncio
    async def test_stream_requests_usage_and_parses_final_chunk(self):
        # OpenAI-compatible streams only carry usage when include_usage is set;
        # the request must ask for it and the final usage-bearing chunk must be
        # surfaced, otherwise cost/context/model status frames never fire.
        provider = _openai_provider()
        usage_chunk = _StreamChunk(finish_reason="stop")
        usage_chunk.usage = MagicMock(prompt_tokens=11, completion_tokens=7)
        chunks = [_StreamChunk(content="hi"), usage_chunk]
        create = AsyncMock(return_value=_AsyncStream(chunks))
        provider._client.chat.completions.create = create
        out = await provider.chat_stream(messages=[{"role": "user", "content": "hi"}])
        assert create.call_args.kwargs["stream_options"] == {"include_usage": True}
        assert out.usage == {"prompt_tokens": 11, "completion_tokens": 7}


# ══════════════════════════════════════════════════════════════════════════════
# Anthropic — chat / parse / param building
# ══════════════════════════════════════════════════════════════════════════════


class TestAnthropicChat:
    @pytest.mark.asyncio
    async def test_chat_happy_path(self):
        provider = _anthropic_provider()
        block = MagicMock()
        block.type = "text"
        block.text = "hi"
        resp = MagicMock()
        resp.content = [block]
        resp.stop_reason = "end_turn"
        resp.model = "claude-sonnet-4-20250514"
        usage = MagicMock()
        usage.input_tokens = 4
        usage.output_tokens = 6
        usage.cache_read_input_tokens = 0
        usage.cache_creation_input_tokens = 0
        resp.usage = usage
        provider._client.messages.create = AsyncMock(return_value=resp)
        out = await provider.chat(messages=[{"role": "user", "content": "hi"}])
        assert out.content == "hi"
        assert out.usage["prompt_tokens"] == 4

    @pytest.mark.asyncio
    async def test_chat_error_mapping(self):
        provider = _anthropic_provider()
        provider._client.messages.create = AsyncMock(side_effect=RuntimeError("ant boom"))
        out = await provider.chat(messages=[{"role": "user", "content": "hi"}])
        assert out.finish_reason == "error"
        assert "ant boom" in out.content

    @pytest.mark.asyncio
    async def test_non_json_200_does_not_leak_attributeerror(self):
        # A custom Anthropic-dialect apiBase reaches the same SDK fallback as the
        # OpenAI path: 200 + text/html yields a plain str, and iterating
        # resp.content on it raised a bare AttributeError with the body dropped.
        provider = _anthropic_provider()
        provider._client.messages.create = AsyncMock(return_value="<html>portal</html>")
        out = await provider.chat(messages=[{"role": "user", "content": "hi"}])
        assert out.finish_reason == "error"
        assert "content" not in out.content.split("has no")[0]
        assert "apiBase" in out.content
        assert "portal" in out.content

    def test_parse_response_tool_use(self):
        provider = _anthropic_provider()
        text_block = MagicMock()
        text_block.type = "text"
        text_block.text = "thinking"
        tool_block = MagicMock()
        tool_block.type = "tool_use"
        tool_block.id = "tu1"
        tool_block.name = "exec"
        tool_block.input = {"cmd": "ls"}
        resp = MagicMock()
        resp.content = [text_block, tool_block]
        resp.stop_reason = "tool_use"
        resp.model = "claude"
        resp.usage = None
        out = provider._parse_response(resp)
        assert out.finish_reason == "tool_calls"
        assert out.tool_calls[0].name == "exec"

    def test_parse_response_keeps_thinking_blocks_verbatim(self):
        """Anthropic validates the signature when a thinking block is replayed,
        so keeping only the text made tool-call continuations unreplayable."""
        provider = _anthropic_provider()
        think = MagicMock()
        think.type = "thinking"
        think.thinking = "let me check"
        think.signature = "SIGabc"
        tool_block = MagicMock()
        tool_block.type = "tool_use"
        tool_block.id = "tu1"
        tool_block.name = "exec"
        tool_block.input = {"cmd": "ls"}
        resp = MagicMock()
        resp.content = [think, tool_block]
        resp.stop_reason = "tool_use"
        resp.model = "claude"
        resp.usage = None
        out = provider._parse_response(resp)
        assert out.reasoning_content == "let me check"
        assert out.thinking_blocks == [
            {"type": "thinking", "thinking": "let me check", "signature": "SIGabc"}]

    def test_parse_response_keeps_redacted_thinking_payload(self):
        """A redacted block carries no readable text — only the opaque data is
        replayable, and dropping it breaks the turn just the same."""
        provider = _anthropic_provider()
        block = MagicMock()
        block.type = "redacted_thinking"
        block.data = "OPAQUE"
        resp = MagicMock()
        resp.content = [block]
        resp.stop_reason = "end_turn"
        resp.model = "claude"
        resp.usage = None
        out = provider._parse_response(resp)
        assert out.thinking_blocks == [{"type": "redacted_thinking", "data": "OPAQUE"}]

    def test_parse_response_keeps_signature_when_text_is_omitted(self):
        """display="omitted" yields an empty thinking string while the signature
        stays mandatory, so an empty-text block must still be carried."""
        provider = _anthropic_provider()
        block = MagicMock()
        block.type = "thinking"
        block.thinking = ""
        block.signature = "SIGxyz"
        resp = MagicMock()
        resp.content = [block]
        resp.stop_reason = "end_turn"
        resp.model = "claude"
        resp.usage = None
        out = provider._parse_response(resp)
        assert out.thinking_blocks == [
            {"type": "thinking", "thinking": "", "signature": "SIGxyz"}]
        assert not out.reasoning_content

    def test_parse_response_without_thinking_leaves_the_field_empty(self):
        provider = _anthropic_provider()
        block = MagicMock()
        block.type = "text"
        block.text = "hi"
        resp = MagicMock()
        resp.content = [block]
        resp.stop_reason = "end_turn"
        resp.model = "claude"
        resp.usage = None
        assert not provider._parse_response(resp).thinking_blocks

    def test_convert_tool_choice_variants(self):
        provider = _anthropic_provider()
        assert provider._convert_tool_choice("auto") == {"type": "auto"}
        assert provider._convert_tool_choice("required") == {"type": "any"}
        assert provider._convert_tool_choice("none") == {"type": "none"}
        assert provider._convert_tool_choice("weird") == {"type": "auto"}
        assert provider._convert_tool_choice({"type": "tool", "name": "x"}) == {
            "type": "tool",
            "name": "x",
        }

    def test_build_params_tool_choice_only_with_tools(self):
        provider = _anthropic_provider()
        tools = [{"function": {"name": "fn", "parameters": {}}}]
        params = provider._build_params(
            "claude-sonnet-4-20250514",
            [{"role": "user", "content": "hi"}],
            tools,
            "auto",
        )
        assert params["tool_choice"] == {"type": "auto"}

    def test_apply_thinking_legacy_budget(self):
        provider = _anthropic_provider(thinking_effort="high")
        params = provider._build_params(
            "claude-3-opus", [{"role": "user", "content": "hi"}], None, None
        )
        assert params["thinking"] == {"type": "enabled", "budget_tokens": 16384}
        assert params["temperature"] == 1

    def test_apply_thinking_adaptive(self):
        provider = _anthropic_provider(thinking_effort="medium")
        params = provider._build_params(
            "claude-opus-4", [{"role": "user", "content": "hi"}], None, None
        )
        assert params["thinking"] == {"type": "adaptive", "display": "summarized"}
        assert params["output_config"]["effort"] == "medium"


class TestAnthropicModelHelpers:
    def test_max_output_for_known_models(self):
        from echo_agent.models.providers.anthropic_provider import _max_output_for_model

        assert _max_output_for_model("claude-opus-4") == 128_000
        assert _max_output_for_model("claude-sonnet-4") == 64_000
        assert _max_output_for_model("claude-3.5-sonnet") == 8192

    def test_max_output_default(self):
        from echo_agent.models.providers.anthropic_provider import _max_output_for_model

        assert _max_output_for_model("unknown-model") == 64_000

    def test_supports_adaptive_thinking(self):
        from echo_agent.models.providers.anthropic_provider import _supports_adaptive_thinking

        assert _supports_adaptive_thinking("claude-opus-4") is True
        assert _supports_adaptive_thinking("claude-3-opus") is False


class TestAnthropicThinkingReplay:
    """The round trip: a parsed thinking block must go back out unchanged."""

    @staticmethod
    def _convert(messages):
        from echo_agent.models.providers.format_utils import openai_to_anthropic_messages

        _, converted = openai_to_anthropic_messages(messages)
        return converted

    def test_thinking_blocks_lead_the_assistant_content(self):
        tb = {"type": "thinking", "thinking": "plan", "signature": "SIG"}
        converted = self._convert([
            {"role": "user", "content": "hi"},
            {"role": "assistant", "content": "", "thinking_blocks": [tb],
             "tool_calls": [{"id": "tu1", "function": {"name": "exec",
                                                       "arguments": '{"cmd":"ls"}'}}]},
            {"role": "tool", "tool_call_id": "tu1", "content": "ok"},
        ])
        blocks = converted[1]["content"]
        assert blocks[0] == tb
        assert blocks[1]["type"] == "tool_use"

    def test_redacted_thinking_is_replayed_too(self):
        tb = {"type": "redacted_thinking", "data": "OPAQUE"}
        converted = self._convert([
            {"role": "user", "content": "hi"},
            {"role": "assistant", "content": "answer", "thinking_blocks": [tb]},
        ])
        assert converted[1]["content"][0] == tb

    def test_unknown_block_types_are_not_replayed(self):
        """Anything the API would reject as an assistant block must be dropped
        rather than forwarded blindly."""
        converted = self._convert([
            {"role": "user", "content": "hi"},
            {"role": "assistant", "content": "answer",
             "thinking_blocks": [{"type": "text", "text": "nope"}, "junk", None]},
        ])
        assert converted[1]["content"] == [{"type": "text", "text": "answer"}]

    def test_a_thinking_only_turn_is_not_padded_as_empty(self):
        tb = {"type": "thinking", "thinking": "plan", "signature": "SIG"}
        converted = self._convert([
            {"role": "user", "content": "hi"},
            {"role": "assistant", "content": "", "thinking_blocks": [tb]},
        ])
        assert converted[1]["content"] == [tb]

    def test_messages_without_thinking_blocks_are_unchanged(self):
        converted = self._convert([
            {"role": "user", "content": "hi"},
            {"role": "assistant", "content": "answer"},
        ])
        assert converted[1]["content"] == [{"type": "text", "text": "answer"}]
