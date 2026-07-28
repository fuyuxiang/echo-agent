"""Provider factory — creates LLMProvider instances from config."""

from __future__ import annotations

import asyncio
import os
import time
from typing import Any, Awaitable

from loguru import logger

from echo_agent.config.schema import ProviderConfig
from echo_agent.models.credential_pool import CredentialPool
from echo_agent.models.provider import (
    LLMProvider,
    StreamDeltaCallback,
    StreamReasoningCallback,
)
from echo_agent.models.rate_limiter import RateLimitedProvider, TokenBucketLimiter

_PROVIDER_MAP: dict[str, str] = {
    "openai": "echo_agent.models.providers.openai_provider.OpenAIProvider",
    "anthropic": "echo_agent.models.providers.anthropic_provider.AnthropicProvider",
    "bedrock": "echo_agent.models.providers.bedrock_provider.BedrockProvider",
    "aws": "echo_agent.models.providers.bedrock_provider.BedrockProvider",
    "gemini": "echo_agent.models.providers.gemini_provider.GeminiProvider",
    "google": "echo_agent.models.providers.gemini_provider.GeminiProvider",
    "openrouter": "echo_agent.models.providers.openrouter_provider.OpenRouterProvider",
}

_API_KEY_ENV: dict[str, tuple[str, ...]] = {
    "openai": ("OPENAI_API_KEY",),
    "anthropic": ("ANTHROPIC_API_KEY",),
    "gemini": ("GOOGLE_API_KEY", "GEMINI_API_KEY"),
    "google": ("GOOGLE_API_KEY", "GEMINI_API_KEY"),
    "openrouter": ("OPENROUTER_API_KEY",),
}

_BEDROCK_PROVIDERS = {"bedrock", "aws"}


def _import_class(dotted_path: str) -> type:
    module_path, class_name = dotted_path.rsplit(".", 1)
    import importlib
    module = importlib.import_module(module_path)
    return getattr(module, class_name)


def _env_api_key(provider_name: str) -> str:
    for env_name in _API_KEY_ENV.get(provider_name, ()):
        value = os.environ.get(env_name, "").strip()
        if value:
            return value
    return ""


def _has_aws_credentials() -> bool:
    return bool(
        os.environ.get("AWS_ACCESS_KEY_ID")
        or os.environ.get("AWS_PROFILE")
        or os.environ.get("AWS_WEB_IDENTITY_TOKEN_FILE")
    )


def _allows_keyless_openai_compatible(provider_name: str, config: ProviderConfig) -> bool:
    if not config.api_base:
        return False
    return provider_name == "openai" or provider_name not in _PROVIDER_MAP


def validate_provider_config(config: ProviderConfig, *, default_model: str = "") -> None:
    provider_name = config.name.lower().strip()
    if not provider_name:
        raise ValueError("provider name is required")

    if provider_name not in _PROVIDER_MAP and not config.api_base:
        raise ValueError(
            f"provider '{config.name}' is OpenAI-compatible by default and requires api_base"
        )

    if not config.models and not default_model:
        raise ValueError(
            f"provider '{config.name}' requires an explicit model; set models.defaultModel "
            "or models.providers[].models"
        )

    if provider_name in _BEDROCK_PROVIDERS:
        if config.api_key or _has_aws_credentials():
            return
        # boto3/AnthropicBedrock can still resolve instance/task role credentials at call time.
        return

    if config.api_key or config.credential_pool or _env_api_key(provider_name):
        return

    if _allows_keyless_openai_compatible(provider_name, config):
        return

    env_hint = ", ".join(_API_KEY_ENV.get(provider_name, ()))
    hint = f" or set {env_hint}" if env_hint else ""
    raise ValueError(f"provider '{config.name}' requires api_key{hint}")


def create_provider(config: ProviderConfig, *, default_model: str = "") -> LLMProvider:
    name = config.name.lower().strip()
    default_model = default_model.strip()
    validate_provider_config(config, default_model=default_model)
    dotted = _PROVIDER_MAP.get(name)

    if dotted:
        cls = _import_class(dotted)
    else:
        from echo_agent.models.providers.openai_provider import OpenAIProvider
        cls = OpenAIProvider
        logger.info("Unknown provider '{}', using OpenAI-compatible mode", name)

    kwargs: dict[str, Any] = {}
    if config.extra_headers:
        kwargs["extra_headers"] = config.extra_headers
    # Only meaningful for the OpenAI-compatible family; other providers accept
    # **kwargs and ignore it. Lets an endpoint that rejects stream_options opt out.
    kwargs["stream_include_usage"] = config.stream_include_usage
    configured_default = default_model or (config.models[0] if config.models else "")
    if configured_default:
        kwargs["default_model"] = configured_default

    pool: CredentialPool | None = None
    if config.credential_pool:
        pool = CredentialPool(config.credential_pool)
        api_key = pool.get_next()
    else:
        api_key = config.api_key or _env_api_key(name)

    provider = cls(api_key=api_key, api_base=config.api_base, **kwargs)
    provider.request_timeout = float(config.timeout_seconds)
    provider.max_retries = int(config.max_retries)

    if pool:
        provider = _PooledProvider(provider, pool, cls, config)

    if config.rate_limit_rpm > 0:
        limiter = TokenBucketLimiter(tokens_per_minute=config.rate_limit_rpm)
        provider = RateLimitedProvider(provider, limiter)

    # The retry wrapper (chat_with_retry) runs on the outermost provider, so
    # the timeout must be visible there too.
    provider.request_timeout = float(config.timeout_seconds)
    provider.max_retries = int(config.max_retries)

    return provider


async def _close_client(client: Any) -> None:
    """Close one SDK/httpx client. Best-effort, mirrors LLMProvider.aclose."""
    closer = getattr(client, "close", None) or getattr(client, "aclose", None)
    if closer is None:
        return
    try:
        result = closer()
        if asyncio.iscoroutine(result) or isinstance(result, Awaitable):
            await result
    except Exception as e:  # pragma: no cover - teardown never surfaces
        logger.debug("Retired pooled client close failed (ignored): {}", e)


class _PooledProvider(LLMProvider):
    """Wraps a provider with credential rotation on errors."""

    # Hard cap on retired-but-not-yet-closed clients. Only reached when
    # rotations come in faster than the grace period drains them; the oldest
    # entries are then closed early rather than letting sockets pile up.
    _RETIRED_CLIENT_LIMIT = 8

    def __init__(self, inner: LLMProvider, pool: CredentialPool, cls: type, config: ProviderConfig):
        super().__init__()
        self._inner = inner
        self._pool = pool
        self._cls = cls
        self._config = config
        self.generation = inner.generation
        # Old clients waiting to be closed once nobody is using them. Rotation
        # can't close a client synchronously: a provider instance is shared by
        # every concurrent session, so the one being replaced may still be
        # serving other in-flight requests (see _rotate_credential). Index-aligned
        # with _retired_deadlines (monotonic clock) — both are FIFO.
        self._retired_clients: list[Any] = []
        self._retired_deadlines: list[float] = []

    def _retirement_grace(self) -> float:
        """Seconds to keep a retired client alive: the longest single request."""
        timeout = getattr(self._inner, "request_timeout", None) or self.request_timeout
        return max(float(timeout or 0.0), 1.0)

    async def _sweep_retired_clients(self, *, force_all: bool = False) -> None:
        """Close retired clients past their grace period (or over the cap)."""
        now = time.monotonic()
        while self._retired_clients:
            over_cap = len(self._retired_clients) > self._RETIRED_CLIENT_LIMIT
            if not (force_all or over_cap or now >= self._retired_deadlines[0]):
                break
            client = self._retired_clients.pop(0)
            self._retired_deadlines.pop(0)
            await _close_client(client)

    async def _rotate_credential(self) -> str:
        """Point _inner at the next key on a FRESH client; retire the old one.

        The previous implementation did ``await self._inner.aclose()`` before
        rebuilding. `_inner` is one object shared across all concurrent sessions,
        so that closed the httpx client that other in-flight requests were still
        reading from — turning one key's rate-limit error into spurious
        "client has been closed" failures on unrelated requests.

        The aclose() call itself can't just be dropped: it exists because an SDK
        client that outlives its loop makes the SDK's ``__del__`` schedule a
        close on whatever loop runs next, surfacing "Event loop is closed" out of
        a task nobody awaits. So the old client is *retired* instead — swapped
        out immediately (new requests get the new key), then closed after a grace
        period covering the longest single request. Waiting for aclose() instead
        would leak a connection pool per rotation: an app-owned provider lives as
        long as the process and never gets closed.
        """
        next_key = self._pool.get_next()
        old_client = getattr(self._inner, "_client", None)
        self._inner.api_key = next_key
        self._inner._client = self._inner._build_client()
        if old_client is not None and old_client is not self._inner._client:
            self._retired_clients.append(old_client)
            self._retired_deadlines.append(time.monotonic() + self._retirement_grace())
        # Sweeping here (and on every request while the list is non-empty) is what
        # keeps rotation from leaking: an app-owned provider lives for the whole
        # process and never gets aclose()d, so waiting for teardown to reclaim
        # these would grow one connection pool per rotation, forever.
        await self._sweep_retired_clients()
        logger.info("Rotated to next credential in pool")
        return next_key

    async def _close_retired_clients(self) -> None:
        """Close every client still waiting, grace period or not (teardown)."""
        await self._sweep_retired_clients(force_all=True)

    async def chat(self, messages, tools=None, model=None, tool_choice=None, **kwargs):
        if self._retired_clients:
            await self._sweep_retired_clients()
        resp = await self._inner.chat(messages, tools, model, tool_choice, **kwargs)
        if resp.finish_reason == "error" and self._pool.size > 1:
            self._pool.report_error(self._inner.api_key)
            next_key = await self._rotate_credential()
            resp = await self._inner.chat(messages, tools, model, tool_choice, **kwargs)
            if resp.finish_reason != "error":
                self._pool.report_success(next_key)
        else:
            self._pool.report_success(self._inner.api_key)
        return resp

    async def chat_stream(
        self,
        messages,
        tools=None,
        model=None,
        tool_choice=None,
        on_delta: StreamDeltaCallback | None = None,
        on_reasoning: StreamReasoningCallback | None = None,
        **kwargs,
    ):
        emitted = False
        if self._retired_clients:
            await self._sweep_retired_clients()

        async def wrapped(delta: str) -> None:
            nonlocal emitted
            emitted = True
            if on_delta:
                await on_delta(delta)

        # A rotation retry re-runs the request on a different key, so the model
        # thinks from the top again. Same rule as the retry wrapper: forward only
        # the first run's reasoning, or the client appends a duplicate trace.
        reasoning_open = True

        async def wrapped_reasoning(delta: str) -> None:
            if on_reasoning and reasoning_open:
                await on_reasoning(delta)

        resp = await self._inner.chat_stream(
            messages,
            tools,
            model,
            tool_choice,
            on_delta=wrapped,
            on_reasoning=wrapped_reasoning if on_reasoning else None,
            **kwargs,
        )
        if resp.finish_reason == "error" and self._pool.size > 1 and not emitted:
            self._pool.report_error(self._inner.api_key)
            next_key = await self._rotate_credential()
            reasoning_open = False
            resp = await self._inner.chat_stream(
                messages,
                tools,
                model,
                tool_choice,
                on_delta=wrapped,
                on_reasoning=wrapped_reasoning if on_reasoning else None,
                **kwargs,
            )
            if resp.finish_reason != "error":
                self._pool.report_success(next_key)
        else:
            self._pool.report_success(self._inner.api_key)
        return resp

    def get_default_model(self) -> str:
        return self._inner.get_default_model()

    async def embed(self, text: str, model: str | None = None) -> list[float] | None:
        # Proxy so embed-capability probes see through the wrapper; without
        # this a pooled embed-capable provider is misdetected as
        # embed-incapable and embedding silently falls back to the local model.
        if not self._inner.supports_embed():
            return None
        return await self._inner.embed(text, model=model)

    def supports_embed(self) -> bool:
        return self._inner.supports_embed()

    async def aclose(self) -> None:
        # The wrapper holds no client of its own; the socket owner is _inner.
        # Retired clients from earlier rotations are closed here too — this is the
        # point where no request can still be holding one, and it keeps the
        # original guarantee that no SDK client outlives its event loop.
        await self._close_retired_clients()
        await self._inner.aclose()
