"""Web tools — fetch URLs and search the web."""

from __future__ import annotations

import asyncio
from typing import Any
from urllib.parse import urlencode, urljoin, urlparse

import aiohttp

from echo_agent.agent.tools.base import Tool, ToolExecutionContext, ToolResult
# SSRF policy lives in security/net_guard.py so the media path enforces the
# identical rules (see that module's docstring). Re-exported under the original
# private names: they are part of this module's de-facto API — browser/actions.py
# and browser/session.py import check_url_ssrf from here, and the test suite
# monkeypatches these attributes.
from echo_agent.security.net_guard import (  # noqa: F401
    _ip_is_blocked,
    check_url_ssrf,
    resolve_and_validate,
)
from echo_agent.security.net_guard import RESOLVE_FAILED_PREFIX as _RESOLVE_FAILED_PREFIX
from echo_agent.security.net_guard import PinnedResolver as _PinnedResolver


def _error_kind_for(exc: BaseException) -> str:
    """Map a transport exception to a ``ToolResult.error_kind``.

    Without this the web tools returned every failure unclassified, so
    ``ToolResult.is_infra_failure`` was always False and the circuit breaker
    never opened on a genuinely unreachable network — repeated timeouts just
    kept costing a tool call each. aiohttp's timeout errors subclass
    ``asyncio.TimeoutError``, so that arm must be checked before ClientError
    (``ConnectionTimeoutError`` is both).
    """
    if isinstance(exc, asyncio.TimeoutError):
        return "timeout"
    if isinstance(exc, (aiohttp.ClientError, OSError)):
        return "dependency"
    return "internal"


class WebFetchTool(Tool):
    name = "web_fetch"
    description = "Fetch content from a URL."
    risk_level = "read_only"
    parameters = {
        "type": "object",
        "properties": {
            "url": {"type": "string", "description": "URL to fetch."},
            "max_chars": {"type": "integer", "description": "Optional acquisition cap; omit to fetch the full page. Oversized content is spilled to disk with a retrieval path."},
        },
        "required": ["url"],
    }
    timeout_seconds = 30
    _MAX_REDIRECTS = 5

    # Per-request budget, kept strictly under the registry's ``timeout_seconds``
    # wait_for. With both deadlines at 30s they raced, and the winner decided
    # whether the failure got classified at all: the outer wait_for reports
    # error_kind="timeout", while a bare inner aiohttp timeout used to report
    # none. Landing inside our own handler keeps classification deterministic.
    _REQUEST_TIMEOUT_MARGIN = 5

    def __init__(self, proxy: str | None = None, allow_private: bool = False):
        self._proxy = proxy
        self._allow_private = allow_private

    def _request_timeout(self) -> float:
        """Per-request deadline that stays inside the registry's wait_for."""
        return max(1.0, self.timeout_seconds - self._REQUEST_TIMEOUT_MARGIN)

    async def execute(self, params: dict[str, Any], ctx: ToolExecutionContext | None = None) -> ToolResult:
        url = params["url"]
        max_chars = min(params.get("max_chars", 2_000_000), 2_000_000)
        try:
            return await self._fetch_with_redirect_guard(url, max_chars)
        except Exception as e:
            return ToolResult(success=False, error=str(e), error_kind=_error_kind_for(e))

    async def _fetch_with_redirect_guard(self, url: str, max_chars: int) -> ToolResult:
        """Fetch with redirects followed manually so every hop is SSRF-checked
        and connected over a pinned IP. aiohttp's automatic redirect handling
        would re-resolve and follow 30x to internal targets unchecked."""
        current = url
        for _hop in range(self._MAX_REDIRECTS + 1):
            connector = None
            if not self._allow_private:
                ips, ssrf_error = await resolve_and_validate(current)
                if ssrf_error:
                    # A resolver failure is an infra fault; an SSRF verdict is not.
                    kind = "dependency" if ssrf_error.startswith(_RESOLVE_FAILED_PREFIX) else ""
                    return ToolResult(success=False, error=ssrf_error, error_kind=kind)
                host = urlparse(current).hostname or ""
                # Pin the validated IPs for the actual connection (anti-rebinding).
                connector = aiohttp.TCPConnector(resolver=_PinnedResolver({host: ips}))
            async with aiohttp.ClientSession(connector=connector) as session:
                async with session.get(
                    current,
                    proxy=self._proxy,
                    allow_redirects=False,
                    timeout=aiohttp.ClientTimeout(total=self._request_timeout()),
                ) as resp:
                    if resp.status in (301, 302, 303, 307, 308):
                        location = resp.headers.get("Location")
                        if not location:
                            return ToolResult(success=False, error=f"HTTP {resp.status} redirect without Location")
                        # Resolve relative redirects against the current URL,
                        # then re-validate on the next loop iteration.
                        current = urljoin(current, location)
                        continue
                    text = await resp.text()
                    original_len = len(text)
                    if len(text) > max_chars:
                        text = text[:max_chars] + f"\n... (truncated, {original_len} total)"
                    content_type = resp.headers.get("content-type", "")
                    header = (
                        f"HTTP {resp.status} {resp.reason or ''}\n"
                        f"URL: {resp.url}\n"
                        f"Content-Type: {content_type}\n\n"
                    )
                    metadata = {"status": resp.status, "url": str(resp.url), "content_type": content_type}
                    if resp.status >= 400:
                        return ToolResult(success=False, error=header + text, metadata=metadata)
                    return ToolResult(output=header + text, metadata=metadata)
        return ToolResult(success=False, error=f"Blocked: exceeded {self._MAX_REDIRECTS} redirects")

    def execution_mode(self, params: dict[str, Any]) -> str:
        return "read_only"


class WebSearchTool(Tool):
    name = "web_search"
    description = "Search the web for information."
    risk_level = "read_only"
    parameters = {
        "type": "object",
        "properties": {
            "query": {"type": "string", "description": "Search query."},
            "max_results": {"type": "integer", "description": "Max results.", "default": 5},
        },
        "required": ["query"],
    }
    timeout_seconds = 30

    def __init__(
        self,
        api_key: str = "",
        *,
        provider: str = "brave",
        api_base: str = "",
        proxy: str | None = None,
        timeout_seconds: int = 30,
    ):
        self._api_key = api_key
        self._provider = provider.lower().strip()
        self._api_base = api_base.strip()
        self._proxy = proxy
        self.timeout_seconds = timeout_seconds

    def is_ready(self) -> bool:
        if self._provider == "searxng":
            return bool(self._api_base)
        return bool(self._api_key)

    def readiness_detail(self) -> tuple[bool, str]:
        if self.is_ready():
            return True, "ok"
        if self._provider == "searxng":
            return False, "searxng api_base not configured"
        return False, f"{self._provider} search API key not configured"

    async def execute(self, params: dict[str, Any], ctx: ToolExecutionContext | None = None) -> ToolResult:
        query = params["query"].strip()
        max_results = max(1, min(int(params.get("max_results", 5)), 20))
        if not query:
            return ToolResult(success=False, error="query is required")
        if self._provider != "searxng" and not self._api_key:
            return ToolResult(success=False, error=f"{self._provider} search API key not configured")

        # A configurable api_base (notably searxng) is operator-controlled but
        # could point at an internal address — validate it like web_fetch.
        if self._api_base:
            ssrf_error = await check_url_ssrf(self._api_base)
            if ssrf_error:
                return ToolResult(success=False, error=ssrf_error, metadata={"provider": self._provider})

        try:
            async with aiohttp.ClientSession(timeout=aiohttp.ClientTimeout(total=self.timeout_seconds)) as session:
                if self._provider == "brave":
                    results = await self._search_brave(session, query, max_results)
                elif self._provider == "tavily":
                    results = await self._search_tavily(session, query, max_results)
                elif self._provider == "serpapi":
                    results = await self._search_serpapi(session, query, max_results)
                elif self._provider == "searxng":
                    results = await self._search_searxng(session, query, max_results)
                elif self._provider == "serply":
                    results = await self._search_serply(session, query, max_results)
                else:
                    return ToolResult(success=False, error=f"Unsupported search provider: {self._provider}")
        except Exception as e:
            return ToolResult(
                success=False, error=str(e), error_kind=_error_kind_for(e),
                metadata={"provider": self._provider},
            )

        if not results:
            return ToolResult(output="No search results.", metadata={"provider": self._provider, "count": 0})

        lines: list[str] = [f"Search results for: {query}"]
        for idx, item in enumerate(results[:max_results], 1):
            title = item.get("title", "").strip() or "(untitled)"
            url = item.get("url", "").strip()
            snippet = item.get("snippet", "").strip()
            lines.append(f"[{idx}] {title}\nURL: {url}\nSnippet: {snippet}".rstrip())
        return ToolResult(
            output="\n\n".join(lines),
            metadata={"provider": self._provider, "count": len(results), "results": results[:max_results]},
        )

    def execution_mode(self, params: dict[str, Any]) -> str:
        return "read_only"

    def _base_url(self, default: str) -> str:
        return self._api_base or default

    async def _read_json(self, resp: aiohttp.ClientResponse) -> dict[str, Any]:
        text = await resp.text()
        if resp.status >= 400:
            raise RuntimeError(f"search provider returned HTTP {resp.status}: {text[:500]}")
        try:
            return await resp.json(content_type=None)
        except Exception as exc:
            raise RuntimeError(f"search provider returned invalid JSON: {text[:500]}") from exc

    async def _search_brave(self, session: aiohttp.ClientSession, query: str, max_results: int) -> list[dict[str, str]]:
        url = self._base_url("https://api.search.brave.com/res/v1/web/search")
        headers = {"Accept": "application/json", "X-Subscription-Token": self._api_key}
        params = {"q": query, "count": max_results}
        async with session.get(url, headers=headers, params=params, proxy=self._proxy) as resp:
            data = await self._read_json(resp)
        raw = data.get("web", {}).get("results", [])
        return [
            {
                "title": item.get("title", ""),
                "url": item.get("url", ""),
                "snippet": item.get("description", ""),
            }
            for item in raw
        ]

    async def _search_tavily(self, session: aiohttp.ClientSession, query: str, max_results: int) -> list[dict[str, str]]:
        url = self._base_url("https://api.tavily.com/search")
        payload = {"api_key": self._api_key, "query": query, "max_results": max_results, "include_answer": False}
        async with session.post(url, json=payload, proxy=self._proxy) as resp:
            data = await self._read_json(resp)
        return [
            {
                "title": item.get("title", ""),
                "url": item.get("url", ""),
                "snippet": item.get("content", ""),
            }
            for item in data.get("results", [])
        ]

    async def _search_serpapi(self, session: aiohttp.ClientSession, query: str, max_results: int) -> list[dict[str, str]]:
        url = self._base_url("https://serpapi.com/search.json")
        params = {"engine": "google", "q": query, "api_key": self._api_key, "num": max_results}
        async with session.get(url, params=params, proxy=self._proxy) as resp:
            data = await self._read_json(resp)
        return [
            {
                "title": item.get("title", ""),
                "url": item.get("link", ""),
                "snippet": item.get("snippet", ""),
            }
            for item in data.get("organic_results", [])
        ]

    async def _search_searxng(self, session: aiohttp.ClientSession, query: str, max_results: int) -> list[dict[str, str]]:
        if not self._api_base:
            raise RuntimeError("SearXNG search_api_base is required")
        url = urljoin(self._api_base.rstrip("/") + "/", "search")
        params = {"q": query, "format": "json", "categories": "general", "language": "auto"}
        async with session.get(url, params=params, proxy=self._proxy) as resp:
            data = await self._read_json(resp)
        return [
            {
                "title": item.get("title", ""),
                "url": item.get("url", ""),
                "snippet": item.get("content", ""),
            }
            for item in data.get("results", [])[:max_results]
        ]

    async def _search_serply(self, session: aiohttp.ClientSession, query: str, max_results: int) -> list[dict[str, str]]:
        # Serply takes the query and result count as URL-encoded path segments.
        base = (self._api_base or "https://api.serply.io").rstrip("/")
        url = f"{base}/v1/search/{urlencode({'q': query, 'num': max_results})}"
        headers = {
            "X-Api-Key": self._api_key,
            "Accept": "application/json",
            # Serply is behind Cloudflare, which rejects the default aiohttp
            # User-Agent, so send an explicit one.
            "User-Agent": "echo-agent",
        }
        async with session.get(url, headers=headers, proxy=self._proxy) as resp:
            data = await self._read_json(resp)
        return [
            {
                "title": item.get("title", ""),
                "url": item.get("link", ""),
                "snippet": item.get("description", ""),
            }
            for item in data.get("results", [])
        ]
