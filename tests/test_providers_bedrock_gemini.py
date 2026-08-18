"""Contract tests for BedrockProvider and GeminiProvider.

All third-party SDKs (boto3, anthropic, google-generativeai) are mocked — no
network access. Focus is on request construction (messages -> provider format),
response parsing (provider response -> LLMResponse), error mapping and token
accounting.
"""

from __future__ import annotations

import base64
from unittest.mock import AsyncMock, MagicMock, patch

import pytest

from echo_agent.models.provider import ToolCallRequest
from echo_agent.models.providers.bedrock_provider import (
    BedrockProvider,
    _is_claude_model,
    _parse_aws_credentials,
    _resolve_region,
)
from echo_agent.models.providers.gemini_provider import GeminiProvider


# ══════════════════════════════════════════════════════════════════════════════
# Bedrock — module-level helpers
# ══════════════════════════════════════════════════════════════════════════════


class TestBedrockCredentialParsing:
    def test_parse_three_parts(self):
        access, secret, region = _parse_aws_credentials("AKIA:SECRET:us-west-2")
        assert access == "AKIA"
        assert secret == "SECRET"
        assert region == "us-west-2"

    def test_parse_two_parts_uses_env_region(self, monkeypatch):
        monkeypatch.setenv("AWS_REGION", "eu-central-1")
        access, secret, region = _parse_aws_credentials("AKIA:SECRET")
        assert access == "AKIA"
        assert secret == "SECRET"
        assert region == "eu-central-1"

    def test_parse_two_parts_default_region(self, monkeypatch):
        monkeypatch.delenv("AWS_REGION", raising=False)
        _, _, region = _parse_aws_credentials("AKIA:SECRET")
        assert region == "us-east-1"

    def test_parse_empty_returns_blanks(self):
        assert _parse_aws_credentials("") == ("", "", "")
        assert _parse_aws_credentials("single") == ("", "", "")

    def test_resolve_region_prefers_aws_region(self, monkeypatch):
        monkeypatch.setenv("AWS_REGION", "ap-south-1")
        monkeypatch.setenv("AWS_DEFAULT_REGION", "us-east-2")
        assert _resolve_region() == "ap-south-1"

    def test_resolve_region_fallback_default(self, monkeypatch):
        monkeypatch.delenv("AWS_REGION", raising=False)
        monkeypatch.delenv("AWS_DEFAULT_REGION", raising=False)
        assert _resolve_region() == "us-east-1"


class TestIsClaudeModel:
    def test_positive(self):
        assert _is_claude_model("anthropic.claude-3-sonnet") is True
        assert _is_claude_model("us.anthropic.claude-3-5") is True
        assert _is_claude_model("CLAUDE-v2") is True

    def test_negative(self):
        assert _is_claude_model("amazon.titan-text") is False
        assert _is_claude_model("meta.llama3-70b") is False


# ══════════════════════════════════════════════════════════════════════════════
# Bedrock — construction / region resolution
# ══════════════════════════════════════════════════════════════════════════════


class TestBedrockInit:
    def test_region_from_explicit_kwarg(self):
        p = BedrockProvider(api_key="AKIA:SECRET:us-west-2", region="eu-west-1")
        assert p._region == "eu-west-1"

    def test_region_from_credentials(self, monkeypatch):
        monkeypatch.delenv("AWS_REGION", raising=False)
        p = BedrockProvider(api_key="AKIA:SECRET:ca-central-1")
        assert p._region == "ca-central-1"
        assert p._access_key == "AKIA"
        assert p._secret_key == "SECRET"

    def test_get_default_model(self):
        p = BedrockProvider(default_model="anthropic.claude-3")
        assert p.get_default_model() == "anthropic.claude-3"


# ══════════════════════════════════════════════════════════════════════════════
# Bedrock — Converse message/tool/system conversion
# ══════════════════════════════════════════════════════════════════════════════


class TestBedrockConverseConversion:
    def _provider(self):
        return BedrockProvider(default_model="amazon.titan-text", region="us-east-1")

    def test_extract_system_string(self):
        p = self._provider()
        parts = p._extract_converse_system(
            [{"role": "system", "content": "be helpful"}, {"role": "user", "content": "hi"}]
        )
        assert parts == [{"text": "be helpful"}]

    def test_extract_system_block_list(self):
        p = self._provider()
        parts = p._extract_converse_system(
            [{"role": "system", "content": [{"type": "text", "text": "sys"}]}]
        )
        assert parts == [{"text": "sys"}]

    def test_to_converse_messages_skips_system(self):
        p = self._provider()
        msgs = p._to_converse_messages(
            [{"role": "system", "content": "x"}, {"role": "user", "content": "hi"}]
        )
        assert len(msgs) == 1
        assert msgs[0]["role"] == "user"
        assert msgs[0]["content"] == [{"text": "hi"}]

    def test_to_converse_messages_assistant_role(self):
        p = self._provider()
        msgs = p._to_converse_messages([{"role": "assistant", "content": "ok"}])
        assert msgs[0]["role"] == "assistant"

    def test_to_converse_messages_tool_calls(self):
        p = self._provider()
        msgs = p._to_converse_messages([
            {
                "role": "assistant",
                "content": "calling",
                "tool_calls": [
                    {"id": "t1", "function": {"name": "exec", "arguments": '{"cmd": "ls"}'}}
                ],
            }
        ])
        content = msgs[0]["content"]
        tool_use = [c for c in content if "toolUse" in c][0]["toolUse"]
        assert tool_use["toolUseId"] == "t1"
        assert tool_use["name"] == "exec"
        assert tool_use["input"] == {"cmd": "ls"}

    def test_to_converse_messages_bad_tool_args_keeps_raw(self):
        p = self._provider()
        msgs = p._to_converse_messages([
            {
                "role": "assistant",
                "content": None,
                "tool_calls": [{"id": "t1", "function": {"name": "x", "arguments": "not-json"}}],
            }
        ])
        tool_use = [c for c in msgs[0]["content"] if "toolUse" in c][0]["toolUse"]
        assert tool_use["input"] == {"raw": "not-json"}

    def test_to_converse_messages_image_block(self):
        p = self._provider()
        raw = base64.b64encode(b"imgbytes").decode()
        url = f"data:image/png;base64,{raw}"
        msgs = p._to_converse_messages([
            {"role": "user", "content": [{"type": "image_url", "image_url": {"url": url}}]}
        ])
        img = msgs[0]["content"][0]["image"]
        assert img["format"] == "png"
        assert img["source"]["bytes"] == b"imgbytes"

    def test_tool_result_merges_into_trailing_user(self):
        p = self._provider()
        result: list = [{"role": "user", "content": [{"text": "q"}]}]
        p._append_converse_tool_result(
            result, {"role": "tool", "tool_call_id": "t1", "content": "done"}
        )
        assert len(result) == 1
        assert result[0]["content"][-1]["toolResult"]["toolUseId"] == "t1"

    def test_tool_result_new_user_when_last_assistant(self):
        p = self._provider()
        result: list = [{"role": "assistant", "content": [{"text": "a"}]}]
        p._append_converse_tool_result(
            result, {"role": "tool", "tool_call_id": "t1", "content": "done"}
        )
        assert len(result) == 2
        assert result[-1]["role"] == "user"

    def test_to_converse_tools(self):
        p = self._provider()
        cfg = p._to_converse_tools([
            {"function": {"name": "fn", "description": "d", "parameters": {"type": "object"}}}
        ])
        spec = cfg["tools"][0]["toolSpec"]
        assert spec["name"] == "fn"
        assert spec["description"] == "d"
        assert spec["inputSchema"]["json"] == {"type": "object"}

    def test_to_converse_tools_defaults_when_no_parameters(self):
        p = self._provider()
        cfg = p._to_converse_tools([{"name": "bare"}])
        spec = cfg["tools"][0]["toolSpec"]
        assert spec["name"] == "bare"
        assert spec["inputSchema"]["json"] == {"type": "object", "properties": {}}


# ══════════════════════════════════════════════════════════════════════════════
# Bedrock — Converse response parsing
# ══════════════════════════════════════════════════════════════════════════════


class TestBedrockConverseParsing:
    def _provider(self):
        return BedrockProvider(default_model="amazon.titan-text", region="us-east-1")

    def test_parse_text_and_usage(self):
        p = self._provider()
        resp = {
            "output": {"message": {"content": [{"text": "hello"}]}},
            "usage": {"inputTokens": 10, "outputTokens": 5},
            "stopReason": "end_turn",
        }
        out = p._parse_converse_response(resp, "amazon.titan-text")
        assert out.content == "hello"
        assert out.finish_reason == "stop"
        assert out.usage == {"prompt_tokens": 10, "completion_tokens": 5}
        assert out.model == "amazon.titan-text"

    def test_parse_tool_use(self):
        p = self._provider()
        resp = {
            "output": {"message": {"content": [
                {"toolUse": {"toolUseId": "tu1", "name": "exec", "input": {"cmd": "ls"}}}
            ]}},
            "stopReason": "tool_use",
        }
        out = p._parse_converse_response(resp, "m")
        assert out.finish_reason == "tool_calls"
        assert out.content is None
        assert out.tool_calls[0] == ToolCallRequest(id="tu1", name="exec", arguments={"cmd": "ls"})

    def test_parse_max_tokens_maps_to_length(self):
        p = self._provider()
        resp = {"output": {"message": {"content": [{"text": "x"}]}}, "stopReason": "max_tokens"}
        out = p._parse_converse_response(resp, "m")
        assert out.finish_reason == "length"

    def test_parse_empty_output(self):
        p = self._provider()
        out = p._parse_converse_response({}, "m")
        assert out.content is None
        assert out.tool_calls == []
        assert out.finish_reason == "stop"


# ══════════════════════════════════════════════════════════════════════════════
# Bedrock — chat() routing + error mapping (clients mocked)
# ══════════════════════════════════════════════════════════════════════════════


class TestBedrockChatRouting:
    @pytest.mark.asyncio
    async def test_chat_routes_claude(self):
        p = BedrockProvider(default_model="anthropic.claude-3", region="us-east-1")
        fake_client = MagicMock()
        text_block = MagicMock()
        text_block.type = "text"
        text_block.text = "hi from claude"
        resp = MagicMock()
        resp.content = [text_block]
        resp.stop_reason = "end_turn"
        resp.model = "anthropic.claude-3"
        usage = MagicMock(spec=["input_tokens", "output_tokens"])
        usage.input_tokens = 3
        usage.output_tokens = 7
        resp.usage = usage
        fake_client.messages.create = AsyncMock(return_value=resp)

        with patch.object(p, "_build_anthropic_bedrock", return_value=fake_client):
            out = await p.chat(messages=[{"role": "user", "content": "hi"}])
        assert out.content == "hi from claude"
        assert out.usage == {"prompt_tokens": 3, "completion_tokens": 7}

    @pytest.mark.asyncio
    async def test_chat_claude_parses_cache_tokens(self):
        # Regression: the Bedrock Claude path now shares parse_anthropic_message
        # with the native provider, so prompt-cache tokens are no longer dropped.
        p = BedrockProvider(default_model="anthropic.claude-3", region="us-east-1")
        fake_client = MagicMock()
        text_block = MagicMock()
        text_block.type = "text"
        text_block.text = "cached"
        resp = MagicMock()
        resp.content = [text_block]
        resp.stop_reason = "end_turn"
        resp.model = "anthropic.claude-3"
        usage = MagicMock(spec=[
            "input_tokens", "output_tokens",
            "cache_read_input_tokens", "cache_creation_input_tokens",
        ])
        usage.input_tokens = 10
        usage.output_tokens = 5
        usage.cache_read_input_tokens = 100
        usage.cache_creation_input_tokens = 20
        resp.usage = usage
        fake_client.messages.create = AsyncMock(return_value=resp)

        with patch.object(p, "_build_anthropic_bedrock", return_value=fake_client):
            out = await p.chat(messages=[{"role": "user", "content": "hi"}])
        # format_utils folds cache_read + cache_creation into cached_tokens;
        # before the shared parser the Bedrock path dropped them entirely.
        assert out.usage.get("cached_tokens") == 120
        assert out.usage.get("prompt_tokens") == 10

    @pytest.mark.asyncio
    async def test_chat_claude_error_mapping(self):
        p = BedrockProvider(default_model="anthropic.claude-3", region="us-east-1")
        fake_client = MagicMock()
        fake_client.messages.create = AsyncMock(side_effect=RuntimeError("boom"))
        with patch.object(p, "_build_anthropic_bedrock", return_value=fake_client):
            out = await p.chat(messages=[{"role": "user", "content": "hi"}])
        assert out.finish_reason == "error"
        assert "boom" in out.content

    @pytest.mark.asyncio
    async def test_chat_claude_passes_tools_and_system(self):
        p = BedrockProvider(default_model="anthropic.claude-3", region="us-east-1")
        fake_client = MagicMock()
        block = MagicMock()
        block.type = "text"
        block.text = "ok"
        resp = MagicMock()
        resp.content = [block]
        resp.stop_reason = "end_turn"
        resp.model = "anthropic.claude-3"
        resp.usage = None
        fake_client.messages.create = AsyncMock(return_value=resp)

        tools = [{"function": {"name": "fn", "description": "d", "parameters": {}}}]
        with patch.object(p, "_build_anthropic_bedrock", return_value=fake_client):
            await p.chat(
                messages=[{"role": "system", "content": "sys"}, {"role": "user", "content": "hi"}],
                tools=tools,
            )
        kwargs = fake_client.messages.create.call_args.kwargs
        assert kwargs["model"] == "anthropic.claude-3"
        assert "system" in kwargs
        assert "tools" in kwargs

    @pytest.mark.asyncio
    async def test_chat_routes_converse(self):
        p = BedrockProvider(default_model="amazon.titan-text", region="us-east-1")
        fake_client = MagicMock()
        fake_client.converse = MagicMock(return_value={
            "output": {"message": {"content": [{"text": "titan reply"}]}},
            "usage": {"inputTokens": 1, "outputTokens": 2},
            "stopReason": "end_turn",
        })
        with patch.object(p, "_build_boto3_client", return_value=fake_client):
            out = await p.chat(messages=[{"role": "user", "content": "hi"}])
        assert out.content == "titan reply"
        assert out.usage["prompt_tokens"] == 1

    @pytest.mark.asyncio
    async def test_chat_converse_error_mapping(self):
        p = BedrockProvider(default_model="amazon.titan-text", region="us-east-1")
        fake_client = MagicMock()
        fake_client.converse = MagicMock(side_effect=RuntimeError("converse fail"))
        with patch.object(p, "_build_boto3_client", return_value=fake_client):
            out = await p.chat(messages=[{"role": "user", "content": "hi"}])
        assert out.finish_reason == "error"
        assert "converse fail" in out.content


# ══════════════════════════════════════════════════════════════════════════════
# Bedrock — client builders raise without SDK installed
# ══════════════════════════════════════════════════════════════════════════════


class TestBedrockClientBuilders:
    def test_anthropic_bedrock_missing_sdk(self, monkeypatch):
        import builtins

        real_import = builtins.__import__

        def fake_import(name, *args, **kwargs):
            if name == "anthropic":
                raise ImportError("no anthropic")
            return real_import(name, *args, **kwargs)

        monkeypatch.setattr(builtins, "__import__", fake_import)
        p = BedrockProvider(default_model="anthropic.claude-3", region="us-east-1")
        with pytest.raises(ImportError):
            p._build_anthropic_bedrock()

    def test_boto3_missing_sdk(self, monkeypatch):
        import builtins

        real_import = builtins.__import__

        def fake_import(name, *args, **kwargs):
            if name == "boto3":
                raise ImportError("no boto3")
            return real_import(name, *args, **kwargs)

        monkeypatch.setattr(builtins, "__import__", fake_import)
        p = BedrockProvider(default_model="amazon.titan", region="us-east-1")
        with pytest.raises(ImportError):
            p._build_boto3_client()


# ══════════════════════════════════════════════════════════════════════════════
# Gemini — construction
# ══════════════════════════════════════════════════════════════════════════════


def _gemini_provider(default_model="gemini-pro"):
    with patch.object(GeminiProvider, "_build_client", return_value=MagicMock()):
        return GeminiProvider(api_key="gem-key", default_model=default_model)


class TestGeminiBuildClient:
    def test_configure_called_with_api_key(self):
        fake_genai = MagicMock()
        fake_google = MagicMock()
        fake_google.generativeai = fake_genai
        with patch.dict("sys.modules", {"google": fake_google, "google.generativeai": fake_genai}):
            p = GeminiProvider(api_key="gem-key", default_model="gemini-pro")
        assert p._client is fake_genai
        fake_genai.configure.assert_called_once_with(api_key="gem-key")

    def test_missing_sdk_raises(self, monkeypatch):
        import builtins

        real_import = builtins.__import__

        def fake_import(name, *args, **kwargs):
            if name == "google.generativeai":
                raise ImportError("no genai")
            return real_import(name, *args, **kwargs)

        monkeypatch.setattr(builtins, "__import__", fake_import)
        with pytest.raises(ImportError):
            GeminiProvider(api_key="x", default_model="gemini-pro")


# ══════════════════════════════════════════════════════════════════════════════
# Gemini — message conversion
# ══════════════════════════════════════════════════════════════════════════════


class TestGeminiConvertMessages:
    def test_system_extracted_separately(self):
        p = _gemini_provider()
        system, contents = p._convert_messages([
            {"role": "system", "content": "be nice"},
            {"role": "user", "content": "hi"},
        ])
        assert system == "be nice"
        assert contents == [{"role": "user", "parts": [{"text": "hi"}]}]

    def test_system_block_list(self):
        p = _gemini_provider()
        system, _ = p._convert_messages([
            {"role": "system", "content": [{"type": "text", "text": "sys"}]}
        ])
        assert system == "sys"

    def test_assistant_maps_to_model_role(self):
        p = _gemini_provider()
        _, contents = p._convert_messages([{"role": "assistant", "content": "answer"}])
        assert contents[0]["role"] == "model"

    def test_adjacent_same_role_merged(self):
        p = _gemini_provider()
        _, contents = p._convert_messages([
            {"role": "user", "content": "a"},
            {"role": "user", "content": "b"},
        ])
        assert len(contents) == 1
        assert contents[0]["parts"] == [{"text": "a"}, {"text": "b"}]

    def test_tool_calls_become_function_call_parts(self):
        p = _gemini_provider()
        _, contents = p._convert_messages([
            {
                "role": "assistant",
                "content": "",
                "tool_calls": [
                    {"function": {"name": "exec", "arguments": '{"cmd": "ls"}'}}
                ],
            }
        ])
        fc = contents[0]["parts"][0]["function_call"]
        assert fc["name"] == "exec"
        assert fc["args"] == {"cmd": "ls"}

    def test_tool_calls_bad_json_defaults_empty(self):
        p = _gemini_provider()
        _, contents = p._convert_messages([
            {
                "role": "assistant",
                "content": "",
                "tool_calls": [{"function": {"name": "x", "arguments": "bad"}}],
            }
        ])
        assert contents[0]["parts"][0]["function_call"]["args"] == {}

    def test_tool_message_becomes_function_response(self):
        p = _gemini_provider()
        _, contents = p._convert_messages([
            {"role": "tool", "name": "exec", "content": "result-data"}
        ])
        fr = contents[0]["parts"][0]["function_response"]
        assert fr["name"] == "exec"
        assert fr["response"] == {"result": "result-data"}
        assert contents[0]["role"] == "user"

    def test_image_block_inline_data(self):
        p = _gemini_provider()
        raw = base64.b64encode(b"img").decode()
        url = f"data:image/jpeg;base64,{raw}"
        _, contents = p._convert_messages([
            {"role": "user", "content": [{"type": "image_url", "image_url": {"url": url}}]}
        ])
        inline = contents[0]["parts"][0]["inline_data"]
        assert inline["mime_type"] == "image/jpeg"
        assert inline["data"] == raw


# ══════════════════════════════════════════════════════════════════════════════
# Gemini — tool conversion
# ══════════════════════════════════════════════════════════════════════════════


class TestGeminiConvertTools:
    def test_function_declarations(self):
        p = _gemini_provider()
        out = p._convert_tools([
            {"function": {"name": "fn", "description": "d", "parameters": {"type": "object"}}}
        ])
        decl = out[0]["function_declarations"][0]
        assert decl["name"] == "fn"
        assert decl["description"] == "d"
        assert decl["parameters"] == {"type": "object"}

    def test_omits_parameters_when_absent(self):
        p = _gemini_provider()
        out = p._convert_tools([{"name": "bare"}])
        decl = out[0]["function_declarations"][0]
        assert "parameters" not in decl


# ══════════════════════════════════════════════════════════════════════════════
# Gemini — response parsing
# ══════════════════════════════════════════════════════════════════════════════


def _gemini_resp(text_parts=(), function_calls=(), usage=None):
    parts = []
    for t in text_parts:
        part = MagicMock(spec=["text", "function_call"])
        part.text = t
        part.function_call = None
        parts.append(part)
    for name, args in function_calls:
        part = MagicMock(spec=["text", "function_call"])
        part.text = None
        fc = MagicMock()
        fc.name = name
        fc.args = args
        part.function_call = fc
        parts.append(part)
    candidate = MagicMock()
    candidate.content.parts = parts
    resp = MagicMock()
    resp.candidates = [candidate]
    if usage is None:
        resp.usage_metadata = None
    else:
        um = MagicMock()
        um.prompt_token_count = usage[0]
        um.candidates_token_count = usage[1]
        resp.usage_metadata = um
    return resp


class TestGeminiParseResponse:
    def test_text_only(self):
        p = _gemini_provider()
        out = p._parse_response(_gemini_resp(text_parts=["hello", "world"]), "gemini-pro")
        assert out.content == "hello\nworld"
        assert out.finish_reason == "stop"
        assert out.model == "gemini-pro"

    def test_function_call(self):
        p = _gemini_provider()
        out = p._parse_response(
            _gemini_resp(function_calls=[("exec", {"cmd": "ls"})]), "gemini-pro"
        )
        assert out.finish_reason == "tool_calls"
        assert out.tool_calls[0].name == "exec"
        assert out.tool_calls[0].arguments == {"cmd": "ls"}
        assert out.tool_calls[0].id == "call_exec"

    def test_usage_metadata(self):
        p = _gemini_provider()
        out = p._parse_response(_gemini_resp(text_parts=["x"], usage=(12, 8)), "gemini-pro")
        assert out.usage == {"prompt_tokens": 12, "completion_tokens": 8}

    def test_empty_content_none(self):
        p = _gemini_provider()
        out = p._parse_response(_gemini_resp(), "gemini-pro")
        assert out.content is None


# ══════════════════════════════════════════════════════════════════════════════
# Gemini — chat() error mapping + happy path (SDK mocked)
# ══════════════════════════════════════════════════════════════════════════════


class TestGeminiChat:
    @pytest.mark.asyncio
    async def test_chat_error_mapping(self):
        p = _gemini_provider()
        genai = MagicMock()
        genai.GenerativeModel.side_effect = RuntimeError("gemini down")
        p._client = genai
        out = await p.chat(messages=[{"role": "user", "content": "hi"}])
        assert out.finish_reason == "error"
        assert "gemini down" in out.content

    @pytest.mark.asyncio
    async def test_chat_happy_path(self):
        p = _gemini_provider()
        genai = MagicMock()
        gemini_model = MagicMock()
        gemini_model.generate_content.return_value = _gemini_resp(text_parts=["reply"])
        genai.GenerativeModel.return_value = gemini_model
        p._client = genai
        out = await p.chat(messages=[{"role": "user", "content": "hi"}])
        assert out.content == "reply"
        model_kwargs = genai.GenerativeModel.call_args.kwargs
        assert "system_instruction" not in model_kwargs

    @pytest.mark.asyncio
    async def test_chat_system_instruction_and_tools(self):
        p = _gemini_provider()
        genai = MagicMock()
        gemini_model = MagicMock()
        gemini_model.generate_content.return_value = _gemini_resp(text_parts=["reply"])
        genai.GenerativeModel.return_value = gemini_model
        p._client = genai
        tools = [{"function": {"name": "fn", "parameters": {"type": "object"}}}]
        await p.chat(
            messages=[{"role": "system", "content": "sys"}, {"role": "user", "content": "hi"}],
            tools=tools,
        )
        model_kwargs = genai.GenerativeModel.call_args.kwargs
        assert model_kwargs["system_instruction"] == "sys"
        send_kwargs = gemini_model.generate_content.call_args.kwargs
        assert "tools" in send_kwargs
