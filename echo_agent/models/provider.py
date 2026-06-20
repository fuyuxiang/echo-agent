"""Model abstraction layer — multi-provider LLM interface.

Supports: multi-model switching, task-based routing, fallback/degradation,
cost control, context length handling, unified generation parameters.
"""

from __future__ import annotations

import asyncio
import json
import random
import re
from collections.abc import Awaitable, Callable
from abc import ABC, abstractmethod
from dataclasses import dataclass, field
from typing import Any

from loguru import logger

StreamDeltaCallback = Callable[[str], Awaitable[None] | None]


async def _invoke_stream_callback(callback: StreamDeltaCallback | None, delta: str) -> None:
    if callback is None or not delta:
        return
    result = callback(delta)
    if asyncio.iscoroutine(result):
        await result


@dataclass
class ToolCallRequest:
    id: str
    name: str
    arguments: dict[str, Any]

    def to_openai_format(self) -> dict[str, Any]:
        return {
            "id": self.id,
            "type": "function",
            "function": {
                "name": self.name,
                "arguments": json.dumps(self.arguments, ensure_ascii=False),
            },
        }


@dataclass
class LLMResponse:
    content: str | None = None
    tool_calls: list[ToolCallRequest] = field(default_factory=list)
    finish_reason: str = "stop"
    usage: dict[str, int] = field(default_factory=dict)
    model: str = ""
    reasoning_content: str | None = None
    thinking_blocks: list[dict[str, Any]] | None = None

    @property
    def has_tool_calls(self) -> bool:
        return len(self.tool_calls) > 0

    @property
    def cache_hit_rate(self) -> float:
        total_input = self.usage.get("input_tokens", 0)
        cache_read = self.usage.get("cache_read_input_tokens", 0)
        if total_input + cache_read == 0:
            return 0.0
        return cache_read / (total_input + cache_read)


@dataclass(frozen=True)
class GenerationParams:
    temperature: float = 0.7
    max_tokens: int = 4096
    top_p: float = 1.0
    reasoning_effort: str | None = None


class LLMProvider(ABC):
    """Abstract base for LLM providers (OpenAI, Anthropic, etc.)."""

    # Status codes are matched with word boundaries so a "429" inside a URL or
    # request id does not misclassify the error.
    _TRANSIENT_CODE_RE = re.compile(r"\b(429|500|502|503|504)\b")
    _TRANSIENT_WORDS = ("rate limit", "overloaded", "timeout", "timed out", "connection reset", "connection error")
    _PERMANENT_CODE_RE = re.compile(r"\b(400|401|403|404|422)\b")
    _PERMANENT_WORDS = ("authentication", "unauthorized", "forbidden", "invalid_api_key", "invalid api key")

    def __init__(self, api_key: str = "", api_base: str = ""):
        self.api_key = api_key
        self.api_base = api_base
        self.generation = GenerationParams()
        # Hard cap per attempt — a stalled request must never hold the
        # per-session lock forever. Wired from ProviderConfig.timeout_seconds.
        self.request_timeout: float = 120.0
        self.max_retries: int = 3

    @abstractmethod
    async def chat(
        self,
        messages: list[dict[str, Any]],
        tools: list[dict[str, Any]] | None = None,
        model: str | None = None,
        tool_choice: str | dict | None = None,
        **kwargs: Any,
    ) -> LLMResponse:
        """Send a chat completion request."""

    @abstractmethod
    def get_default_model(self) -> str:
        """Return the default model identifier."""

    async def embed(self, text: str, model: str | None = None) -> list[float] | None:
        """Generate an embedding vector for *text*. Returns None when unsupported."""
        return None

    async def chat_stream(
        self,
        messages: list[dict[str, Any]],
        tools: list[dict[str, Any]] | None = None,
        model: str | None = None,
        tool_choice: str | dict | None = None,
        on_delta: StreamDeltaCallback | None = None,
        **kwargs: Any,
    ) -> LLMResponse:
        """Stream text deltas when supported, otherwise fall back to a full response."""
        response = await self.chat(messages, tools, model, tool_choice, **kwargs)
        if response.finish_reason != "error" and response.content:
            await _invoke_stream_callback(on_delta, response.content)
        return response

    def _is_transient(self, error_text: str) -> bool:
        return self._classify_error_text(error_text) == "transient"

    def _is_permanent(self, error_text: str) -> bool:
        return self._classify_error_text(error_text) == "permanent"

    def _classify_error_text(self, error_text: str) -> str:
        """Classify an error message as 'transient', 'permanent' or 'unknown'."""
        lower = error_text.lower()
        if self._PERMANENT_CODE_RE.search(lower) or any(m in lower for m in self._PERMANENT_WORDS):
            return "permanent"
        if self._TRANSIENT_CODE_RE.search(lower) or any(m in lower for m in self._TRANSIENT_WORDS):
            return "transient"
        return "unknown"

    @staticmethod
    def _status_code_of(e: Exception) -> int | None:
        """Extract an HTTP status code from SDK exceptions when available."""
        code = getattr(e, "status_code", None)
        if isinstance(code, int):
            return code
        resp = getattr(e, "response", None)
        for attr in ("status_code", "status"):
            code = getattr(resp, attr, None)
            if isinstance(code, int):
                return code
        return None

    def _classify_exception(self, e: Exception) -> str:
        """Classify a raised exception — prefer typed status codes over text."""
        code = self._status_code_of(e)
        if code is not None:
            if code == 429 or code >= 500:
                return "transient"
            if 400 <= code < 500:
                return "permanent"
        if isinstance(e, (TimeoutError, ConnectionError, asyncio.TimeoutError)):
            return "transient"
        return self._classify_error_text(str(e))

    @staticmethod
    def _is_empty_success(response: LLMResponse) -> bool:
        # finish=stop yet nothing came back: the model "succeeded" but produced
        # no answer and no tool call. Treat as a one-shot retryable failure.
        return (
            response.finish_reason == "stop"
            and not response.content
            and not response.has_tool_calls
        )

    @property
    def is_stub(self) -> bool:
        return False

    @property
    def _stream_timeout(self) -> float | None:
        # Streams legitimately run longer than a unary call; still bound them
        # so a stalled stream cannot brick the session.
        return self.request_timeout * 5 if self.request_timeout else None

    def _retry_delays(self) -> list[float]:
        # Exponential backoff base delays; length == retry attempts.
        return [float(2 ** i) for i in range(max(1, self.max_retries))]

    async def chat_with_retry(self, **kwargs: Any) -> LLMResponse:
        timeout = self.request_timeout or None
        for attempt, base_delay in enumerate(self._retry_delays()):
            try:
                response = await asyncio.wait_for(self.chat(**kwargs), timeout=timeout)
            except asyncio.CancelledError:
                raise
            except (TimeoutError, asyncio.TimeoutError):
                response = LLMResponse(content=f"Error: request timed out after {timeout}s", finish_reason="error")
                classification = "transient"
            except Exception as e:
                response = LLMResponse(content=f"Error: {e}", finish_reason="error")
                classification = self._classify_exception(e)
            else:
                if response.finish_reason != "error":
                    if self._is_empty_success(response):
                        retried = await self._retry_empty_once(self.chat, timeout, kwargs)
                        return retried if retried is not None else response
                    return response
                classification = self._classify_error_text(response.content or "")

            if classification != "transient":
                return response

            jitter = base_delay * (0.5 + random.random())
            logger.warning("LLM transient error (attempt {}), retrying in {:.1f}s", attempt + 1, jitter)
            await asyncio.sleep(jitter)

        try:
            return await asyncio.wait_for(self.chat(**kwargs), timeout=timeout)
        except asyncio.CancelledError:
            raise
        except Exception as e:
            return LLMResponse(content=f"Error: {e}", finish_reason="error")

    async def _retry_empty_once(self, caller, timeout, kwargs):
        # One extra attempt for an empty "stop". Returns the retry response if it
        # produced something usable, else None so the caller keeps the original.
        logger.warning("LLM returned empty content on finish=stop, retrying once")
        try:
            retry = await asyncio.wait_for(caller(**kwargs), timeout=timeout)
        except asyncio.CancelledError:
            raise
        except Exception:
            return None
        if retry.finish_reason != "error" and not self._is_empty_success(retry):
            return retry
        return None

    async def chat_stream_with_retry(
        self,
        messages: list[dict[str, Any]],
        tools: list[dict[str, Any]] | None = None,
        model: str | None = None,
        tool_choice: str | dict | None = None,
        on_delta: StreamDeltaCallback | None = None,
        **kwargs: Any,
    ) -> LLMResponse:
        emitted = False
        timeout = self._stream_timeout

        async def wrapped(delta: str) -> None:
            nonlocal emitted
            emitted = True
            await _invoke_stream_callback(on_delta, delta)

        def _stream_call() -> Awaitable[LLMResponse]:
            return self.chat_stream(
                messages=messages,
                tools=tools,
                model=model,
                tool_choice=tool_choice,
                on_delta=wrapped,
                **kwargs,
            )

        for attempt, base_delay in enumerate(self._retry_delays()):
            emitted = False
            try:
                response = await asyncio.wait_for(_stream_call(), timeout=timeout)
            except asyncio.CancelledError:
                raise
            except (TimeoutError, asyncio.TimeoutError):
                response = LLMResponse(content=f"Error: stream timed out after {timeout}s", finish_reason="error")
                classification = "transient"
            except Exception as e:
                response = LLMResponse(content=f"Error: {e}", finish_reason="error")
                classification = self._classify_exception(e)
            else:
                if response.finish_reason != "error":
                    if not emitted and self._is_empty_success(response):
                        logger.warning("LLM stream returned empty content on finish=stop, retrying once")
                        try:
                            retry = await asyncio.wait_for(_stream_call(), timeout=timeout)
                        except asyncio.CancelledError:
                            raise
                        except Exception:
                            return response
                        if retry.finish_reason != "error" and not self._is_empty_success(retry):
                            return retry
                        return response
                    return response
                classification = self._classify_error_text(response.content or "")

            # Never retry after partial emission — the user already saw tokens.
            if emitted or classification != "transient":
                return response

            jitter = base_delay * (0.5 + random.random())
            logger.warning("LLM transient stream error (attempt {}), retrying in {:.1f}s", attempt + 1, jitter)
            await asyncio.sleep(jitter)

        try:
            return await asyncio.wait_for(_stream_call(), timeout=timeout)
        except asyncio.CancelledError:
            raise
        except Exception as e:
            return LLMResponse(content=f"Error: {e}", finish_reason="error")
