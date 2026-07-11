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


@dataclass
class VerifyResult:
    status: str  # "ok" | "error" | "unreachable"
    detail: str = ""


_UNREACHABLE = (httpx.ConnectError, httpx.ConnectTimeout, httpx.ReadTimeout,
                httpx.TimeoutException, ConnectionError, TimeoutError)


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
                return VerifyResult("error", "provider returned error")
            return VerifyResult("ok")
        except _UNREACHABLE as e:
            return VerifyResult("unreachable", str(e))
        except Exception as e:
            return VerifyResult("error", str(e))

    try:
        return asyncio.run(_probe())
    except _UNREACHABLE as e:
        return VerifyResult("unreachable", str(e))
    except Exception as e:
        return VerifyResult("error", str(e))
