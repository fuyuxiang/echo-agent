"""Google Gemini provider — generative AI SDK."""

from __future__ import annotations

import json
from typing import Any

from loguru import logger

from echo_agent.models.provider import (
    LLMProvider,
    LLMResponse,
    StreamDeltaCallback,
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
    def __init__(self, parts: list[_AggregatePart]):
        self.content = _AggregateContent(parts)


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
        text_buf: list[str] = []
        fc_parts: list[_AggregatePart] = []
        usage: Any = None

        for chunk in chunks:
            chunk_usage = getattr(chunk, "usage_metadata", None)
            if chunk_usage:
                usage = chunk_usage
            for candidate in getattr(chunk, "candidates", None) or []:
                content = getattr(candidate, "content", None)
                for part in getattr(content, "parts", None) or []:
                    text = getattr(part, "text", "") or ""
                    if text:
                        text_buf.append(text)
                    fc = getattr(part, "function_call", None)
                    if fc:
                        fc_parts.append(_AggregatePart(function_call=fc))

        parts: list[_AggregatePart] = []
        if text_buf:
            parts.append(_AggregatePart(text="".join(text_buf)))
        parts.extend(fc_parts)

        self.candidates = [_AggregateCandidate(parts)]
        if usage is not None:
            self.usage_metadata = usage


class GeminiProvider(LLMProvider):

    def __init__(self, api_key: str = "", api_base: str = "", default_model: str = "", **kwargs: Any):
        super().__init__(api_key=api_key, api_base=api_base)
        self._default_model = default_model
        self._client = self._build_client()

    def _build_client(self) -> Any:
        try:
            import google.generativeai as genai
        except ImportError:
            raise ImportError("google-generativeai required: pip install echo-agent[gemini]")
        if self.api_key:
            genai.configure(api_key=self.api_key)
        return genai

    async def chat(
        self,
        messages: list[dict[str, Any]],
        tools: list[dict[str, Any]] | None = None,
        model: str | None = None,
        tool_choice: str | dict | None = None,
        **kwargs: Any,
    ) -> LLMResponse:
        target = model or self._default_model
        try:
            return await self._do_chat(target, messages, tools, **kwargs)
        except Exception as e:
            logger.error("Gemini API error: {}", e)
            return LLMResponse(content=f"Error: {e}", finish_reason="error")

    def get_default_model(self) -> str:
        return self._default_model

    async def chat_stream(
        self,
        messages: list[dict[str, Any]],
        tools: list[dict[str, Any]] | None = None,
        model: str | None = None,
        tool_choice: str | dict | None = None,
        on_delta: StreamDeltaCallback | None = None,
        **kwargs: Any,
    ) -> LLMResponse:
        import asyncio

        target = model or self._default_model
        genai = self._client
        system_text, contents = self._convert_messages(messages)
        gen_config = {
            "temperature": kwargs.get("temperature", self.generation.temperature),
            "max_output_tokens": kwargs.get("max_tokens", self.generation.max_tokens),
        }
        model_kwargs: dict[str, Any] = {"model_name": target, "generation_config": gen_config}
        if system_text:
            model_kwargs["system_instruction"] = system_text
        gemini_model = genai.GenerativeModel(**model_kwargs)

        send_kwargs: dict[str, Any] = {"content": contents, "stream": True}
        tool_defs = self._convert_tools(tools) if tools else None
        if tool_defs:
            send_kwargs["tools"] = tool_defs

        loop = asyncio.get_running_loop()
        try:
            # The Gemini SDK is synchronous: kick off the streaming call and pull
            # each chunk via run_in_executor so the event loop stays responsive
            # and deltas reach on_delta as they arrive (true streaming).
            stream = await loop.run_in_executor(None, lambda: gemini_model.generate_content(**send_kwargs))
            chunks: list[Any] = []
            sentinel = object()
            it = iter(stream)
            while True:
                chunk = await loop.run_in_executor(None, lambda: next(it, sentinel))
                if chunk is sentinel:
                    break
                chunks.append(chunk)
                # `chunk.text` is a property that can raise (not just be missing)
                # on non-pure-text chunks carrying a function_call, so guard the
                # access itself: treat any failure as empty text and skip it.
                try:
                    text = getattr(chunk, "text", "") or ""
                except Exception:
                    text = ""
                if text:
                    await _invoke_stream_callback(on_delta, text)
            resp = _GeminiAggregate(chunks)
        except Exception as e:
            logger.error("Gemini stream error: {}", e)
            return LLMResponse(content=f"Error: {e}", finish_reason="error")
        return self._parse_response(resp, target)

    async def _do_chat(
        self, model_name: str, messages: list[dict[str, Any]],
        tools: list[dict[str, Any]] | None, **kwargs: Any,
    ) -> LLMResponse:
        import asyncio
        genai = self._client

        system_text, contents = self._convert_messages(messages)
        gen_config = {
            "temperature": kwargs.get("temperature", self.generation.temperature),
            "max_output_tokens": kwargs.get("max_tokens", self.generation.max_tokens),
        }

        model_kwargs: dict[str, Any] = {"model_name": model_name, "generation_config": gen_config}
        if system_text:
            model_kwargs["system_instruction"] = system_text

        gemini_model = genai.GenerativeModel(**model_kwargs)

        tool_defs = self._convert_tools(tools) if tools else None
        send_kwargs: dict[str, Any] = {"content": contents}
        if tool_defs:
            send_kwargs["tools"] = tool_defs

        resp = await asyncio.get_running_loop().run_in_executor(
            None, lambda: gemini_model.generate_content(**send_kwargs),
        )
        return self._parse_response(resp, model_name)

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
                parts.append({"function_call": {"name": fn.get("name", ""), "args": args}})

            if role == "tool":
                parts = [{"function_response": {
                    "name": msg.get("name", ""),
                    "response": {"result": content},
                }}]
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
                decl["parameters"] = params
            declarations.append(decl)
        return [{"function_declarations": declarations}]

    def _parse_response(self, resp: Any, model: str) -> LLMResponse:
        text_parts: list[str] = []
        tool_calls: list[ToolCallRequest] = []

        for candidate in resp.candidates:
            for part in candidate.content.parts:
                if hasattr(part, "text") and part.text:
                    text_parts.append(part.text)
                if hasattr(part, "function_call") and part.function_call:
                    fc = part.function_call
                    args = dict(fc.args) if fc.args else {}
                    tool_calls.append(ToolCallRequest(
                        id=f"call_{fc.name}",
                        name=fc.name,
                        arguments=args,
                    ))

        finish = "tool_calls" if tool_calls else "stop"
        usage: dict[str, int] = {}
        if hasattr(resp, "usage_metadata") and resp.usage_metadata:
            um = resp.usage_metadata
            usage["prompt_tokens"] = getattr(um, "prompt_token_count", 0) or 0
            usage["completion_tokens"] = getattr(um, "candidates_token_count", 0) or 0

        return LLMResponse(
            content="\n".join(text_parts) if text_parts else None,
            tool_calls=tool_calls,
            finish_reason=finish,
            usage=usage,
            model=model,
        )
