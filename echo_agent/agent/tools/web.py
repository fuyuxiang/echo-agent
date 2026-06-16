"""Web tools — fetch URLs and search the web."""

from __future__ import annotations

import asyncio
import ipaddress
import socket
from typing import Any
from urllib.parse import urljoin, urlparse

import aiohttp

from echo_agent.agent.tools.base import Tool, ToolExecutionContext, ToolResult


def _ip_is_blocked(ip: ipaddress._BaseAddress) -> bool:
    return bool(
        ip.is_private or ip.is_loopback or ip.is_link_local
        or ip.is_reserved or ip.is_multicast or ip.is_unspecified
    )


async def resolve_and_validate(url: str) -> tuple[list[str], str | None]:
    """Resolve *url*'s host and validate every resolved IP.

    Returns ``(ips, error)``. ``ips`` is the list of validated address
    strings (safe to pin a connection to); ``error`` is non-None when the
    URL must be blocked. Pinning the returned IPs for the actual connection
    closes the DNS-rebinding window between validation and connect.
    """
    parsed = urlparse(url)
    if parsed.scheme not in ("http", "https"):
        return [], f"Blocked: unsupported URL scheme '{parsed.scheme}'"
    host = parsed.hostname
    if not host:
        return [], "Blocked: URL has no host"
    try:
        infos = await asyncio.to_thread(socket.getaddrinfo, host, None)
    except OSError as e:
        return [], f"Blocked: cannot resolve host '{host}': {e}"
    ips: list[str] = []
    for info in infos:
        addr = info[4][0]
        try:
            ip = ipaddress.ip_address(addr)
        except ValueError:
            continue
        if _ip_is_blocked(ip):
            return [], (
                f"Blocked: '{host}' resolves to non-public address {ip}. "
                "Set tools.web.allowPrivateAddresses to true to permit internal targets."
            )
        ips.append(addr)
    if not ips:
        return [], f"Blocked: cannot resolve host '{host}' to a usable address"
    return ips, None


async def check_url_ssrf(url: str) -> str | None:
    """Return an error message when *url* points at a non-public address.

    Blocks loopback, private, link-local (cloud metadata), reserved and
    multicast targets — web_fetch takes model-controlled URLs, so without
    this it is a free proxy into the host's internal network.
    """
    _, error = await resolve_and_validate(url)
    return error


class _PinnedResolver(aiohttp.abc.AbstractResolver):
    """aiohttp resolver that hands back a pre-validated IP for a host.

    Pins DNS so the address aiohttp connects to is exactly the one SSRF
    validation approved, defeating rebinding (validate IP_a, connect IP_b)."""

    def __init__(self, host_to_ips: dict[str, list[str]]):
        self._map = host_to_ips

    async def resolve(self, host: str, port: int = 0, family: int = socket.AF_INET) -> list[dict[str, Any]]:
        ips = self._map.get(host)
        if not ips:
            raise OSError(f"host '{host}' not in pinned set")
        results: list[dict[str, Any]] = []
        for addr in ips:
            try:
                fam = socket.AF_INET6 if ipaddress.ip_address(addr).version == 6 else socket.AF_INET
            except ValueError:
                continue
            if family not in (socket.AF_UNSPEC, fam):
                continue
            results.append({
                "hostname": host, "host": addr, "port": port,
                "family": fam, "proto": 0, "flags": socket.AI_NUMERICHOST,
            })
        if not results:
            raise OSError(f"no pinned address for '{host}' in family {family}")
        return results

    async def close(self) -> None:
        return None



class WebFetchTool(Tool):
    name = "web_fetch"
    description = "Fetch content from a URL."
    risk_level = "read_only"
    parameters = {
        "type": "object",
        "properties": {
            "url": {"type": "string", "description": "URL to fetch."},
            "max_chars": {"type": "integer", "description": "Max response chars.", "default": 16000},
        },
        "required": ["url"],
    }
    timeout_seconds = 30
    _MAX_REDIRECTS = 5

    def __init__(self, proxy: str | None = None, allow_private: bool = False):
        self._proxy = proxy
        self._allow_private = allow_private

    async def execute(self, params: dict[str, Any], ctx: ToolExecutionContext | None = None) -> ToolResult:
        url = params["url"]
        max_chars = params.get("max_chars", 16000)
        try:
            return await self._fetch_with_redirect_guard(url, max_chars)
        except Exception as e:
            return ToolResult(success=False, error=str(e))

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
                    return ToolResult(success=False, error=ssrf_error)
                host = urlparse(current).hostname or ""
                # Pin the validated IPs for the actual connection (anti-rebinding).
                connector = aiohttp.TCPConnector(resolver=_PinnedResolver({host: ips}))
            async with aiohttp.ClientSession(connector=connector) as session:
                async with session.get(
                    current,
                    proxy=self._proxy,
                    allow_redirects=False,
                    timeout=aiohttp.ClientTimeout(total=self.timeout_seconds),
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
                else:
                    return ToolResult(success=False, error=f"Unsupported search provider: {self._provider}")
        except Exception as e:
            return ToolResult(success=False, error=str(e), metadata={"provider": self._provider})

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
