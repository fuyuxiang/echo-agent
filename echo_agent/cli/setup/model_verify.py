"""Dynamic model listing and live model verification for setup.

Both operations are best-effort: any network/parse failure degrades quietly
(list_models -> [], verify_model -> unreachable) so the wizard never blocks.
"""
from __future__ import annotations

import asyncio
from dataclasses import dataclass

import httpx

from echo_agent.cli.setup.providers import ProviderCatalogEntry
from echo_agent.config.schema import ProviderConfig
from echo_agent.models.providers import create_provider

DEFAULT_TIMEOUT = 8.0


def list_models(entry: ProviderCatalogEntry, api_key: str, api_base: str = "") -> list[str]:
    endpoint = entry.models_endpoint
    if not endpoint:
        return []
    if api_base and entry.api_base and endpoint.startswith(entry.api_base):
        endpoint = api_base.rstrip("/") + endpoint[len(entry.api_base.rstrip("/")):]
    headers = {}
    if api_key:
        if entry.dialect == "anthropic":
            headers["x-api-key"] = api_key
            headers["anthropic-version"] = "2023-06-01"
        else:
            headers["Authorization"] = f"Bearer {api_key}"
    params = {}
    if entry.dialect == "gemini" and api_key:
        params["key"] = api_key
        headers.pop("Authorization", None)
    try:
        resp = httpx.get(endpoint, headers=headers, params=params, timeout=DEFAULT_TIMEOUT)
        resp.raise_for_status()
        body = resp.json()
    except Exception:
        return []
    return _parse_models(entry.dialect, body)


def _parse_models(dialect: str, body: dict) -> list[str]:
    try:
        if dialect == "gemini":
            out = []
            for m in body.get("models", []):
                name = (m.get("name") or "").split("/")[-1]
                if name:
                    out.append(name)
            return out
        if dialect == "anthropic":
            return [m.get("id") for m in body.get("data", []) if m.get("id")]
        # openai / openrouter and compatibles
        return [m.get("id") for m in body.get("data", []) if m.get("id")]
    except Exception:
        return []


def list_model_windows(entry: ProviderCatalogEntry, api_key: str, api_base: str = "") -> dict[str, int]:
    """Best-effort per-model context windows from the provider's /models listing.

    Mirrors list_models but keeps the window metadata that _parse_models drops
    (OpenRouter context_length, Gemini inputTokenLimit). Providers that don't
    report a window (e.g. OpenAI native /models) yield an empty dict, and the
    built-in registry takes over. Any failure degrades to {} — never blocks setup.
    """
    endpoint = entry.models_endpoint
    if not endpoint:
        return {}
    if api_base and entry.api_base and endpoint.startswith(entry.api_base):
        endpoint = api_base.rstrip("/") + endpoint[len(entry.api_base.rstrip("/")):]
    headers = {}
    if api_key:
        if entry.dialect == "anthropic":
            headers["x-api-key"] = api_key
            headers["anthropic-version"] = "2023-06-01"
        else:
            headers["Authorization"] = f"Bearer {api_key}"
    params = {}
    if entry.dialect == "gemini" and api_key:
        params["key"] = api_key
        headers.pop("Authorization", None)
    try:
        resp = httpx.get(endpoint, headers=headers, params=params, timeout=DEFAULT_TIMEOUT)
        resp.raise_for_status()
        body = resp.json()
    except Exception:
        return {}
    return _parse_model_windows(entry.dialect, body)


# Field names OpenAI-compatible /models endpoints use for a model's context
# window. Different providers pick different keys, so we probe a set rather than
# a single name (mirrors hermes-agent's _CONTEXT_LENGTH_KEYS). Ordered by how
# authoritative/common the key is; first positive hit wins.
_CONTEXT_WINDOW_KEYS = (
    "context_length",
    "context_window",
    "context_size",
    "max_context_length",
    "max_model_len",
    "max_input_tokens",
    "max_sequence_length",
    "max_seq_len",
)


def _first_window(payload: dict) -> int:
    """First positive context-window value among the known keys. 0 if none."""
    for key in _CONTEXT_WINDOW_KEYS:
        val = payload.get(key)
        if isinstance(val, (int, float)) and val > 0:
            return int(val)
    return 0


def _parse_model_windows(dialect: str, body: dict) -> dict[str, int]:
    out: dict[str, int] = {}
    try:
        if dialect == "gemini":
            for m in body.get("models", []):
                name = (m.get("name") or "").split("/")[-1]
                limit = m.get("inputTokenLimit")
                if name and isinstance(limit, (int, float)) and limit > 0:
                    out[name] = int(limit)
            return out
        # openai / openrouter / anthropic and compatibles use a "data" list.
        for m in body.get("data", []):
            mid = m.get("id")
            if not mid:
                continue
            # Probe the common window keys at the top level, then fall back to
            # OpenRouter's nested top_provider.context_length.
            win = _first_window(m)
            if not win:
                tp = m.get("top_provider") or {}
                if isinstance(tp, dict):
                    win = _first_window(tp)
            if win > 0:
                out[mid] = win
        return out
    except Exception:
        return {}


@dataclass
class VerifyResult:
    status: str  # "ok" | "error" | "unreachable"
    detail: str = ""


_UNREACHABLE = (httpx.ConnectError, httpx.ConnectTimeout, httpx.ReadTimeout,
                httpx.TimeoutException, ConnectionError, TimeoutError)

# Substrings (case-insensitive) that mark an error as a connectivity/timeout
# failure rather than a provider-side (auth/quota/bad-request) error.
_UNREACHABLE_HINTS = ("timed out", "timeout", "connect", "connection", "network",
                      "unreachable", "dns", "getaddrinfo")


def _classify_error_detail(content: str) -> str:
    """Classify an error LLMResponse's content into "unreachable" or "error".

    ``chat_with_retry`` never raises; it returns ``finish_reason="error"`` with
    the failure text in ``content``. Connectivity/timeout failures map to
    "unreachable", everything else (auth, quota, bad request) to "error".
    """
    text = (content or "").lower()
    if any(hint in text for hint in _UNREACHABLE_HINTS):
        return "unreachable"
    return "error"


def verify_model(dialect: str, api_key: str, api_base: str, model: str) -> VerifyResult:
    try:
        cfg = ProviderConfig(name=dialect, api_key=api_key, api_base=api_base,
                             models=[model], timeout_seconds=int(DEFAULT_TIMEOUT))
        provider = create_provider(cfg, default_model=model)
    except _UNREACHABLE as e:
        return VerifyResult("unreachable", str(e))
    except Exception as e:
        return VerifyResult("error", str(e))

    async def _probe() -> VerifyResult:
        try:
            resp = await provider.chat_with_retry(
                messages=[{"role": "user", "content": "ping"}], model=model,
            )
            if getattr(resp, "finish_reason", "") == "error":
                content = getattr(resp, "content", "") or ""
                return VerifyResult(_classify_error_detail(content), content)
            return VerifyResult("ok")
        except _UNREACHABLE as e:
            return VerifyResult("unreachable", str(e))
        except Exception as e:
            return VerifyResult("error", str(e))
        finally:
            # Must happen inside this loop: the SDK's httpx.AsyncClient pooled
            # its sockets here, and asyncio.run() closes the loop on return. A
            # client left open is finalized later by the SDK's __del__, which
            # schedules aclose() on whichever loop is then running — the setup
            # wizard's prompt_toolkit loop — and tearing down a dead loop's
            # transports raises "Event loop is closed" from an unawaited task,
            # printed as "Unhandled exception in event loop" mid-wizard.
            # Teardown must never change the verdict, so it swallows everything
            # (a duck-typed provider may not even have aclose).
            try:
                await provider.aclose()
            except Exception:
                pass

    try:
        return asyncio.run(_probe())
    except _UNREACHABLE as e:
        return VerifyResult("unreachable", str(e))
    except Exception as e:
        return VerifyResult("error", str(e))
