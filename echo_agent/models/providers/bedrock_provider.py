"""AWS Bedrock provider — Claude via AnthropicBedrock, others via Converse API."""

from __future__ import annotations

import json
import os
from typing import Any

from loguru import logger

from echo_agent.models.provider import (
    LLMProvider,
    LLMResponse,
    StreamDeltaCallback,
    StreamReasoningCallback,
    ToolCallRequest,
)
from echo_agent.models.providers.anthropic_provider import (
    parse_anthropic_message,
    stream_anthropic_messages,
)
from echo_agent.models.providers.format_utils import (
    openai_to_anthropic_messages,
    openai_to_anthropic_tools,
)


def _is_claude_model(model: str) -> bool:
    return "anthropic." in model or "claude" in model.lower()


def _parse_aws_credentials(api_key: str) -> tuple[str, str, str]:
    parts = api_key.split(":")
    if len(parts) >= 3:
        return parts[0], parts[1], parts[2]
    if len(parts) == 2:
        return parts[0], parts[1], os.environ.get("AWS_REGION", "us-east-1")
    return "", "", ""


def _resolve_region() -> str:
    return os.environ.get("AWS_REGION") or os.environ.get("AWS_DEFAULT_REGION") or "us-east-1"


class BedrockProvider(LLMProvider):

    def __init__(self, api_key: str = "", api_base: str = "", default_model: str = "", **kwargs: Any):
        super().__init__(api_key=api_key, api_base=api_base)
        self._default_model = default_model
        self._region = kwargs.get("region", "")
        self._profile = kwargs.get("profile", "")
        self._access_key, self._secret_key, region_from_key = _parse_aws_credentials(api_key)
        if not self._region:
            self._region = region_from_key or _resolve_region()

    async def chat(
        self,
        messages: list[dict[str, Any]],
        tools: list[dict[str, Any]] | None = None,
        model: str | None = None,
        tool_choice: str | dict | None = None,
        **kwargs: Any,
    ) -> LLMResponse:
        target = model or self._default_model
        if _is_claude_model(target):
            return await self._chat_claude(target, messages, tools, tool_choice, **kwargs)
        return await self._chat_converse(target, messages, tools, tool_choice, **kwargs)

    def get_default_model(self) -> str:
        return self._default_model

    async def chat_stream(
        self,
        messages: list[dict[str, Any]],
        tools: list[dict[str, Any]] | None = None,
        model: str | None = None,
        tool_choice: str | dict | None = None,
        on_delta: "StreamDeltaCallback | None" = None,
        on_reasoning: "StreamReasoningCallback | None" = None,
        **kwargs: Any,
    ) -> LLMResponse:
        target = model or self._default_model
        if not _is_claude_model(target):
            # The Converse streaming path only parses text deltas; it cannot
            # collect toolUse blocks. When tools are supplied the model may
            # respond with a tool call, so fall back to the non-streaming
            # _chat_converse, which fully parses tool_calls/usage/stopReason.
            # Correctness (never dropping tool calls) takes priority over the
            # per-token streaming feel here — tool-bearing turns are typically
            # not ones the user is watching stream. Without tools we keep the
            # true streaming path.
            if tools:
                return await self._chat_converse(
                    target, messages, tools, tool_choice, **kwargs
                )
            return await self._chat_stream_converse(
                target, messages, tools, tool_choice, on_delta=on_delta, **kwargs
            )

        client = self._build_anthropic_bedrock()
        system_blocks, converted = openai_to_anthropic_messages(messages)
        params: dict[str, Any] = {
            "model": target,
            "messages": converted,
            "max_tokens": kwargs.get("max_tokens", self.generation.max_tokens),
        }
        if system_blocks:
            params["system"] = system_blocks
        if tools:
            params["tools"] = openai_to_anthropic_tools(tools)
        temp = kwargs.get("temperature", self.generation.temperature)
        if temp is not None:
            params["temperature"] = temp

        try:
            final = await stream_anthropic_messages(
                client, params, on_delta, on_reasoning
            )
        except Exception as e:
            logger.error("Bedrock Claude stream error: {}", e)
            return LLMResponse(content=f"Error: {e}", finish_reason="error")

        # Shared parser keeps block extraction and usage (incl. prompt-cache
        # tokens) consistent with the native Anthropic provider.
        return parse_anthropic_message(final)

    async def _chat_stream_converse(
        self, model: str, messages: list[dict[str, Any]],
        tools: list[dict[str, Any]] | None, tool_choice: str | dict | None,
        *, on_delta: "StreamDeltaCallback | None" = None, **kwargs: Any,
    ) -> LLMResponse:
        import asyncio

        from echo_agent.models.provider import _invoke_stream_callback
        client = self._build_boto3_client()
        converse_msgs = self._to_converse_messages(messages)
        system_parts = self._extract_converse_system(messages)

        params: dict[str, Any] = {"modelId": model, "messages": converse_msgs}
        if system_parts:
            params["system"] = system_parts
        if tools:
            params["toolConfig"] = self._to_converse_tools(tools)

        config: dict[str, Any] = {"maxTokens": kwargs.get("max_tokens", self.generation.max_tokens)}
        temp = kwargs.get("temperature", self.generation.temperature)
        if temp is not None:
            config["temperature"] = temp
        params["inferenceConfig"] = config

        loop = asyncio.get_running_loop()
        text_parts: list[str] = []
        stop_reason = ""
        usage: dict[str, int] = {}
        try:
            resp = await loop.run_in_executor(None, lambda: client.converse_stream(**params))
            # boto3 EventStream is a synchronous iterator; pull one event at a
            # time on the executor so each delta is emitted as it arrives rather
            # than buffered until the stream is exhausted.
            events = resp.get("stream", []) if isinstance(resp, dict) else []
            it = iter(events)
            sentinel = object()
            while True:
                ev = await loop.run_in_executor(None, lambda: next(it, sentinel))
                if ev is sentinel:
                    break
                # Defend per event: a malformed block must not abort the stream.
                try:
                    if not isinstance(ev, dict):
                        continue
                    block = ev.get("contentBlockDelta")
                    if block:
                        text = (block or {}).get("delta", {}).get("text", "")
                        if text:
                            text_parts.append(text)
                            await _invoke_stream_callback(on_delta, text)
                    # messageStop carries the stop reason; metadata carries token
                    # usage. Capture both so streamed turns report finish_reason
                    # and usage just like the non-streaming _chat_converse path —
                    # otherwise cost tracking silently records zero.
                    stop_ev = ev.get("messageStop")
                    if stop_ev:
                        stop_reason = stop_ev.get("stopReason", "") or stop_reason
                    meta = ev.get("metadata")
                    if meta and meta.get("usage"):
                        u = meta["usage"]
                        usage["prompt_tokens"] = u.get("inputTokens", 0)
                        usage["completion_tokens"] = u.get("outputTokens", 0)
                except Exception:
                    continue
        except Exception as e:
            logger.error("Bedrock converse stream error: {}", e)
            return LLMResponse(content=f"Error: {e}", finish_reason="error")

        # Map stopReason → finish_reason the same way _chat_converse does, so a
        # max_tokens truncation is not misreported as a clean stop. The text-only
        # stream path never collects toolUse (the tool case falls back to the
        # non-streaming converse upstream), so tool_use is not expected here.
        finish = "length" if stop_reason == "max_tokens" else "stop"
        return LLMResponse(
            content="".join(text_parts),
            finish_reason=finish,
            usage=usage,
            model=model,
        )

    async def _chat_claude(
        self, model: str, messages: list[dict[str, Any]],
        tools: list[dict[str, Any]] | None, tool_choice: str | dict | None, **kwargs: Any,
    ) -> LLMResponse:
        client = self._build_anthropic_bedrock()
        system_blocks, converted = openai_to_anthropic_messages(messages)

        params: dict[str, Any] = {
            "model": model,
            "messages": converted,
            "max_tokens": kwargs.get("max_tokens", self.generation.max_tokens),
        }
        if system_blocks:
            params["system"] = system_blocks
        if tools:
            params["tools"] = openai_to_anthropic_tools(tools)
        temp = kwargs.get("temperature", self.generation.temperature)
        if temp is not None:
            params["temperature"] = temp

        try:
            resp = await client.messages.create(**params)
        except Exception as e:
            logger.error("Bedrock Claude error: {}", e)
            return LLMResponse(content=f"Error: {e}", finish_reason="error")

        return parse_anthropic_message(resp)

    def _build_anthropic_bedrock(self) -> Any:
        try:
            from anthropic import AsyncAnthropicBedrock
        except ImportError:
            raise ImportError("anthropic SDK required for Bedrock Claude: pip install echo-agent[bedrock]")

        kwargs: dict[str, Any] = {"aws_region": self._region}
        if self._access_key and self._secret_key:
            kwargs["aws_access_key"] = self._access_key
            kwargs["aws_secret_key"] = self._secret_key
        if self._profile:
            kwargs["aws_profile"] = self._profile
        return AsyncAnthropicBedrock(**kwargs)

    async def _chat_converse(
        self, model: str, messages: list[dict[str, Any]],
        tools: list[dict[str, Any]] | None, tool_choice: str | dict | None, **kwargs: Any,
    ) -> LLMResponse:
        client = self._build_boto3_client()
        converse_msgs = self._to_converse_messages(messages)
        system_parts = self._extract_converse_system(messages)

        params: dict[str, Any] = {"modelId": model, "messages": converse_msgs}
        if system_parts:
            params["system"] = system_parts
        if tools:
            params["toolConfig"] = self._to_converse_tools(tools)

        config: dict[str, Any] = {"maxTokens": kwargs.get("max_tokens", self.generation.max_tokens)}
        temp = kwargs.get("temperature", self.generation.temperature)
        if temp is not None:
            config["temperature"] = temp
        params["inferenceConfig"] = config

        try:
            import asyncio
            resp = await asyncio.get_running_loop().run_in_executor(None, lambda: client.converse(**params))
        except Exception as e:
            logger.error("Bedrock Converse error: {}", e)
            return LLMResponse(content=f"Error: {e}", finish_reason="error")

        return self._parse_converse_response(resp, model)

    def _build_boto3_client(self) -> Any:
        try:
            import boto3
        except ImportError:
            raise ImportError("boto3 required for Bedrock Converse: pip install echo-agent[bedrock]")

        kwargs: dict[str, Any] = {"region_name": self._region}
        if self._access_key and self._secret_key:
            kwargs["aws_access_key_id"] = self._access_key
            kwargs["aws_secret_access_key"] = self._secret_key
        if self._profile:
            session = boto3.Session(profile_name=self._profile)
            return session.client("bedrock-runtime", **{k: v for k, v in kwargs.items() if k != "aws_access_key_id" and k != "aws_secret_access_key"})
        return boto3.client("bedrock-runtime", **kwargs)

    def _extract_converse_system(self, messages: list[dict[str, Any]]) -> list[dict[str, Any]]:
        parts = []
        for msg in messages:
            if msg.get("role") == "system":
                content = msg.get("content", "")
                if isinstance(content, list):
                    text = " ".join(
                        b.get("text", "") for b in content
                        if isinstance(b, dict) and b.get("type") == "text"
                    )
                else:
                    text = content or ""
                parts.append({"text": text})
        return parts

    def _to_converse_messages(self, messages: list[dict[str, Any]]) -> list[dict[str, Any]]:
        result = []
        for msg in messages:
            role = msg.get("role", "")
            if role == "system":
                continue
            if role == "tool":
                self._append_converse_tool_result(result, msg)
                continue

            converse_role = "assistant" if role == "assistant" else "user"
            content_parts = []

            text = msg.get("content")
            if text:
                if isinstance(text, list):
                    for block in text:
                        if isinstance(block, dict):
                            if block.get("type") == "text":
                                content_parts.append({"text": block.get("text", "")})
                            elif block.get("type") == "image_url":
                                url = (block.get("image_url") or {}).get("url", "")
                                if url.startswith("data:"):
                                    import base64 as b64mod
                                    mime, _, raw = url.partition(";")
                                    mime = mime.replace("data:", "")
                                    _, _, b64_data = raw.partition(",")
                                    content_parts.append({
                                        "image": {"format": mime.split("/")[-1], "source": {"bytes": b64mod.b64decode(b64_data)}}
                                    })
                        else:
                            content_parts.append({"text": str(block)})
                else:
                    content_parts.append({"text": text})

            for tc in msg.get("tool_calls", []):
                fn = tc.get("function", {})
                args_str = fn.get("arguments", "{}")
                try:
                    args = json.loads(args_str) if isinstance(args_str, str) else args_str
                except (json.JSONDecodeError, TypeError):
                    args = {"raw": args_str}
                content_parts.append({
                    "toolUse": {"toolUseId": tc.get("id", ""), "name": fn.get("name", ""), "input": args}
                })

            if content_parts:
                result.append({"role": converse_role, "content": content_parts})
        return result

    def _append_converse_tool_result(self, result: list[dict[str, Any]], msg: dict[str, Any]) -> None:
        block = {
            "toolResult": {
                "toolUseId": msg.get("tool_call_id", ""),
                "content": [{"text": msg.get("content", "")}],
            }
        }
        if result and result[-1].get("role") == "user":
            result[-1]["content"].append(block)
        else:
            result.append({"role": "user", "content": [block]})

    def _to_converse_tools(self, tools: list[dict[str, Any]]) -> dict[str, Any]:
        tool_specs = []
        for tool in tools:
            fn = tool.get("function", tool)
            tool_specs.append({
                "toolSpec": {
                    "name": fn.get("name", ""),
                    "description": fn.get("description", ""),
                    "inputSchema": {"json": fn.get("parameters", {"type": "object", "properties": {}})},
                }
            })
        return {"tools": tool_specs}

    def _parse_converse_response(self, resp: dict[str, Any], model: str) -> LLMResponse:
        output = resp.get("output", {})
        message = output.get("message", {})
        content_parts = message.get("content", [])

        text_parts: list[str] = []
        tool_calls: list[ToolCallRequest] = []

        for part in content_parts:
            if "text" in part:
                text_parts.append(part["text"])
            elif "toolUse" in part:
                tu = part["toolUse"]
                tool_calls.append(ToolCallRequest(
                    id=tu.get("toolUseId", ""),
                    name=tu.get("name", ""),
                    arguments=tu.get("input", {}),
                ))

        usage: dict[str, int] = {}
        u = resp.get("usage", {})
        if u:
            usage["prompt_tokens"] = u.get("inputTokens", 0)
            usage["completion_tokens"] = u.get("outputTokens", 0)

        stop = resp.get("stopReason", "end_turn")
        finish = "tool_calls" if stop == "tool_use" else ("length" if stop == "max_tokens" else "stop")

        return LLMResponse(
            content="\n".join(text_parts) if text_parts else None,
            tool_calls=tool_calls,
            finish_reason=finish,
            usage=usage,
            model=model,
        )
