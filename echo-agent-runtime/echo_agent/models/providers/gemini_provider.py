"""Google Gemini provider — generative AI SDK."""

from __future__ import annotations

import json
import uuid
from contextlib import aclosing
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


class _AggregatePart:
    """A single synthetic content part — text or function_call."""

    def __init__(self, text: str = "", function_call: Any = None):
        self.text = text
        self.function_call = function_call


class _AggregateContent:
    def __init__(self, parts: list[_AggregatePart]):
        self.parts = parts


class _AggregateCandidate:
    def __init__(self, parts: list[_AggregatePart], finish_reason: Any = None):
        self.content = _AggregateContent(parts)
        self.finish_reason = finish_reason


class _GeminiAggregate:
    """Adapt streamed chunks into the shape `_parse_response` expects.

    `_parse_response` reads `resp.candidates[*].content.parts[*].text` /
    `.function_call` and `resp.usage_metadata` — it never touches `resp.text`.
    Streamed chunks split text across many parts/chunks and may carry
    `function_call`s and `usage_metadata` only on later chunks, so we flatten
    everything into a single synthetic candidate: text is concatenated into one
    part (otherwise `_parse_response`'s "\\n".join would inject spurious
    newlines between fragments), and every function_call is preserved in order.
    """

    def __init__(self, chunks: list[Any]):
        self.native_parts: list[dict[str, Any]] = []
        text_buf: list[str] = []
        thought_buf: list[str] = []
        fc_parts: list[_AggregatePart] = []
        usage: Any = None
        finish_reason: Any = None
        prompt_feedback: Any = None
        had_candidate = False

        for chunk in chunks:
            prompt_feedback = getattr(chunk, "prompt_feedback", None) or prompt_feedback
            chunk_usage = getattr(chunk, "usage_metadata", None)
            if chunk_usage:
                usage = chunk_usage
            for candidate in (getattr(chunk, "candidates", None) or [])[:1]:
                had_candidate = True
                candidate_finish = getattr(candidate, "finish_reason", None)
                if candidate_finish is not None:
                    finish_reason = candidate_finish
                content = getattr(candidate, "content", None)
                for part in getattr(content, "parts", None) or []:
                    text = getattr(part, "text", "") or ""
                    if callable(getattr(part, "model_dump", None)):
                        self.native_parts.append(part.model_dump(mode="json", exclude_none=True))
                    if text:
                        (thought_buf if getattr(part, "thought", False) is True else text_buf).append(text)
                    fc = getattr(part, "function_call", None)
                    if fc:
                        fc_parts.append(_AggregatePart(function_call=fc))

        parts: list[_AggregatePart] = []
        if text_buf:
            parts.append(_AggregatePart(text="".join(text_buf)))
        if thought_buf:
            thought_part = _AggregatePart(text="".join(thought_buf))
            thought_part.thought = True
            parts.append(thought_part)
        parts.extend(fc_parts)

        self.candidates = [_AggregateCandidate(parts, finish_reason)] if had_candidate else []
        self.prompt_feedback = prompt_feedback
        if usage is not None:
            self.usage_metadata = usage


class GeminiProvider(LLMProvider):

    def __init__(self, api_key: str = "", api_base: str = "", default_model: str = "", **kwargs: Any):
        super().__init__(api_key=api_key, api_base=api_base)
        self._default_model = default_model
        self._extra_headers = dict(kwargs.get("extra_headers", {}))
        self._client = self._build_client()

    def _build_client(self) -> Any:
        try:
            from google import genai
        except ImportError as exc:
            raise ImportError("google-genai required: pip install echo-agent[gemini]") from exc
        options: dict[str, Any] = {}
        if self.api_base:
            options["base_url"] = self.api_base.rstrip("/")
        if self._extra_headers:
            options["headers"] = self._extra_headers
        # Return the async owner itself: generic credential retirement and
        # provider shutdown can aclose it without leaking a synchronous client.
        return genai.Client(api_key=self.api_key, http_options=options or None, vertexai=False).aio

    def get_default_model(self) -> str:
        return self._default_model

    def _request(self, messages, tools, model, tool_choice, kwargs):
        system, contents = self._convert_messages(messages)
        config: dict[str, Any] = {
            "temperature": kwargs.get("temperature", self.generation.temperature),
            "max_output_tokens": kwargs.get("max_tokens", self.generation.max_tokens),
            "top_p": kwargs.get("top_p", self.generation.top_p),
            # Only Echo's ToolRegistry may execute tools, after authorization.
            "automatic_function_calling": {"disable": True},
        }
        if system:
            config["system_instruction"] = system
        if tools:
            config["tools"] = self._convert_tools(tools)
            modes = {"auto": "AUTO", "none": "NONE", "required": "ANY"}
            if isinstance(tool_choice, dict):
                name = tool_choice.get("function", {}).get("name")
                if not name:
                    raise ValueError("Gemini tool_choice requires a function name")
                choice = {"mode": "ANY", "allowed_function_names": [name]}
            else:
                if tool_choice is not None and tool_choice not in modes:
                    raise ValueError("Unsupported Gemini tool_choice")
                choice = {"mode": modes.get(tool_choice, "AUTO")}
            config["tool_config"] = {"function_calling_config": choice}
        for key in ("response_mime_type", "response_json_schema"):
            if key in kwargs:
                config[key] = kwargs[key]
        return {"model": model or self._default_model, "contents": contents, "config": config}

    async def chat(self, messages, tools=None, model=None, tool_choice=None, **kwargs) -> LLMResponse:
        try:
            request = self._request(messages, tools, model, tool_choice, kwargs)
            response = await self._client.models.generate_content(**request)
            return self._parse_response(response, request["model"])
        except Exception as exc:
            logger.error("Gemini API error: {}", exc)
            return LLMResponse(content=f"Error: {exc}", finish_reason="error")

    async def chat_stream(
        self, messages, tools=None, model=None, tool_choice=None,
        on_delta: StreamDeltaCallback | None = None,
        on_reasoning: StreamReasoningCallback | None = None, **kwargs,
    ) -> LLMResponse:
        try:
            request = self._request(messages, tools, model, tool_choice, kwargs)
            stream = await self._client.models.generate_content_stream(**request)
            chunks = []
            # Closing the response iterator on cancellation also closes its
            # HTTP stream; no background executor thread can continue billing.
            async with aclosing(stream):
                async for chunk in stream:
                    chunks.append(chunk)
                    for candidate in (getattr(chunk, "candidates", None) or [])[:1]:
                        for part in getattr(getattr(candidate, "content", None), "parts", None) or []:
                            text = getattr(part, "text", None)
                            if text:
                                callback = on_reasoning if getattr(part, "thought", False) else on_delta
                                await _invoke_stream_callback(callback, text)
            return self._parse_response(_GeminiAggregate(chunks), request["model"])
        except Exception as exc:
            logger.error("Gemini stream error: {}", exc)
            return LLMResponse(content=f"Error: {exc}", finish_reason="error")

    def _convert_messages(self, messages: list[dict[str, Any]]) -> tuple[str, list[dict[str, Any]]]:
        system_parts: list[str] = []
        contents: list[dict[str, Any]] = []

        for msg in messages:
            role = msg.get("role", "")
            content = msg.get("content", "")

            if role == "system":
                if isinstance(content, list):
                    text = " ".join(
                        b.get("text", "") for b in content
                        if isinstance(b, dict) and b.get("type") == "text"
                    )
                else:
                    text = content or ""
                system_parts.append(text)
                continue

            gemini_role = "model" if role == "assistant" else "user"
            parts: list[dict[str, Any]] = []

            native = (msg.get("provider_content") or {}).get("gemini")
            if role == "assistant" and native:
                contents.append({"role": "model", "parts": native})
                continue

            if content:
                if isinstance(content, list):
                    for block in content:
                        if isinstance(block, dict):
                            if block.get("type") == "text":
                                parts.append({"text": block.get("text", "")})
                            elif block.get("type") == "image_url":
                                url = (block.get("image_url") or {}).get("url", "")
                                if url.startswith("data:"):
                                    mime, _, raw = url.partition(";")
                                    mime = mime.replace("data:", "")
                                    _, _, b64 = raw.partition(",")
                                    parts.append({"inline_data": {"mime_type": mime, "data": b64}})
                                elif url:
                                    raise ValueError("Gemini images must use inline data URLs; remote URLs are not fetched")
                        else:
                            parts.append({"text": str(block)})
                else:
                    parts.append({"text": content})

            for tc in msg.get("tool_calls", []):
                fn = tc.get("function", {})
                args_str = fn.get("arguments", "{}")
                try:
                    args = json.loads(args_str) if isinstance(args_str, str) else args_str
                except (json.JSONDecodeError, TypeError):
                    args = {}
                call = {"name": fn.get("name", ""), "args": args}
                if tc.get("id"):
                    call["id"] = tc["id"]
                parts.append({"function_call": call})

            if role == "tool":
                parts = [{"function_response": {
                    "name": msg.get("name", ""),
                    "response": {"result": content},
                }}]
                if msg.get("tool_call_id"):
                    parts[0]["function_response"]["id"] = msg["tool_call_id"]
                gemini_role = "user"

            if parts:
                if contents and contents[-1]["role"] == gemini_role:
                    contents[-1]["parts"].extend(parts)
                else:
                    contents.append({"role": gemini_role, "parts": parts})

        return "\n".join(system_parts), contents

    def _convert_tools(self, tools: list[dict[str, Any]]) -> list[dict[str, Any]]:
        declarations = []
        for tool in tools:
            fn = tool.get("function", tool)
            decl: dict[str, Any] = {"name": fn.get("name", ""), "description": fn.get("description", "")}
            params = fn.get("parameters")
            if params:
                decl["parameters_json_schema"] = params
            declarations.append(decl)
        return [{"function_declarations": declarations}]

    def _parse_response(self, resp: Any, model: str) -> LLMResponse:
        text_parts: list[str] = []
        reasoning: list[str] = []
        tool_calls: list[ToolCallRequest] = []
        native_parts: list[dict[str, Any]] = []
        candidates = getattr(resp, "candidates", None) or []
        raw_finish = None
        # candidate_count is one; never merge alternative candidates into a
        # single answer or execute mutually exclusive alternatives' tools.
        for candidate in candidates[:1]:
            raw_finish = getattr(candidate, "finish_reason", None)
            for part in getattr(getattr(candidate, "content", None), "parts", None) or []:
                if callable(getattr(part, "model_dump", None)):
                    native_parts.append(part.model_dump(mode="json", exclude_none=True))
                text = getattr(part, "text", None)
                if text:
                    (reasoning if getattr(part, "thought", False) is True else text_parts).append(text)
                fc = getattr(part, "function_call", None)
                if fc:
                    call_id = getattr(fc, "id", None)
                    tool_calls.append(ToolCallRequest(
                        id=call_id if isinstance(call_id, str) and call_id else f"call_{uuid.uuid4().hex}",
                        name=fc.name, arguments=dict(fc.args or {}),
                    ))
        finish = self._normalize_finish_reason(raw_finish)
        feedback = getattr(resp, "prompt_feedback", None)
        if not candidates and getattr(feedback, "block_reason", None):
            finish = "content_filter"
        elif not candidates:
            finish = "error"
        if tool_calls and finish == "stop":
            finish = "tool_calls"
        usage: dict[str, int] = {}
        um = getattr(resp, "usage_metadata", None)
        if um:
            usage["prompt_tokens"] = getattr(um, "prompt_token_count", 0) or 0
            usage["completion_tokens"] = getattr(um, "candidates_token_count", 0) or 0
            # Some SDK test doubles expose arbitrary attributes; counts must be ints.
            for source, dest in (("thoughts_token_count", "reasoning_tokens"),
                                 ("cached_content_token_count", "cache_read_input_tokens")):
                value = getattr(um, source, None)
                if isinstance(value, int) and value:
                    usage[dest] = value
        aggregated_parts = getattr(resp, "native_parts", None)
        if isinstance(aggregated_parts, list) and aggregated_parts:
            native_parts = aggregated_parts
        return LLMResponse(
            content="\n".join(text_parts) if text_parts else None,
            reasoning_content="\n".join(reasoning) if reasoning else None,
            tool_calls=tool_calls, finish_reason=finish,
            raw_finish_reason=str(raw_finish or ""), usage=usage, model=model,
            provider_content={"gemini": native_parts} if native_parts else None,
        )

    @staticmethod
    def _normalize_finish_reason(raw: Any) -> str:
        """Map Gemini SDK enum/string variants onto Echo's provider contract."""
        if raw is None:
            return "stop"
        name = getattr(raw, "name", None)
        value = str(name or raw).rsplit(".", 1)[-1].strip().lower()
        if value in {"max_tokens", "max_output_tokens"}:
            return "length"
        if value in {
            "safety", "recitation", "blocklist", "prohibited_content",
            "spii", "image_safety", "image_prohibited_content",
        }:
            return "content_filter"
        if value in {"malformed_function_call", "unexpected_tool_call", "other", "no_image"}:
            return "error"
        return "stop"
