"""web_fetch / web_search failure classification.

The web tools used to return every failure with an empty ``error_kind``, so
``ToolResult.is_infra_failure`` was always False and the circuit breaker never
opened on an unreachable network — repeated timeouts each cost a full tool call.
These pin the classification so an infra fault is reportable as one, while a
policy verdict (SSRF block, missing API key) stays a business failure.
"""

from __future__ import annotations

import asyncio
import socket
from unittest.mock import patch

import aiohttp
import pytest

from echo_agent.agent.tools.web import WebFetchTool, WebSearchTool


def _addrinfo(*ips: str):
    return [(socket.AF_INET, socket.SOCK_STREAM, 6, "", (ip, 0)) for ip in ips]


class _Boom(WebFetchTool):
    """web_fetch whose transport raises *exc* instead of hitting the network."""

    def __init__(self, exc: BaseException):
        super().__init__()
        self._exc = exc

    async def _fetch_with_redirect_guard(self, url, max_chars):
        raise self._exc


# ── web_fetch: infra faults are classified ───────────────────────────────────


@pytest.mark.asyncio
@pytest.mark.parametrize("exc", [
    asyncio.TimeoutError(),
    aiohttp.ServerTimeoutError("read timeout"),
])
async def test_timeouts_classified_as_timeout(exc):
    result = await _Boom(exc).execute({"url": "https://example.com/"})
    assert not result.success
    assert result.error_kind == "timeout"
    assert result.is_infra_failure


@pytest.mark.asyncio
@pytest.mark.parametrize("exc_factory", [
    lambda: aiohttp.ClientConnectorError(
        aiohttp.client_reqrep.ConnectionKey(
            "example.com", 443, False, True, None, None, None,
        ),
        OSError("connection refused"),
    ),
    lambda: aiohttp.ClientPayloadError("truncated body"),
    lambda: aiohttp.ClientError("generic transport failure"),
    lambda: OSError("network unreachable"),
])
async def test_transport_errors_classified_as_dependency(exc_factory):
    result = await _Boom(exc_factory()).execute({"url": "https://example.com/"})
    assert not result.success
    assert result.error_kind == "dependency"
    assert result.is_infra_failure


@pytest.mark.asyncio
async def test_unexpected_error_classified_as_internal():
    result = await _Boom(ValueError("bad state")).execute({"url": "https://example.com/"})
    assert result.error_kind == "internal"
    assert result.is_infra_failure


@pytest.mark.asyncio
async def test_connection_timeout_prefers_timeout_over_dependency():
    """ConnectionTimeoutError is both a TimeoutError and a ClientError; the
    timeout arm must win so the two kinds stay distinguishable."""
    exc_cls = getattr(aiohttp, "ConnectionTimeoutError", None)
    if exc_cls is None:  # pragma: no cover - older aiohttp
        pytest.skip("aiohttp has no ConnectionTimeoutError")
    assert issubclass(exc_cls, asyncio.TimeoutError)
    result = await _Boom(exc_cls("connect timeout")).execute({"url": "https://example.com/"})
    assert result.error_kind == "timeout"


# ── web_fetch: policy verdicts stay business failures ────────────────────────


@pytest.mark.asyncio
async def test_ssrf_block_is_not_an_infra_failure():
    """A blocked private target is a stable verdict — retrying cannot fix it,
    so it must not open the circuit for every other web_fetch caller."""
    tool = WebFetchTool()
    with patch("socket.getaddrinfo", return_value=_addrinfo("10.0.0.5")):
        result = await tool.execute({"url": "http://internal.test/"})
    assert not result.success
    assert result.error_kind == ""
    assert not result.is_infra_failure


@pytest.mark.asyncio
async def test_dns_resolution_failure_is_a_dependency_failure():
    """Resolver breakage is an infra fault, unlike an SSRF verdict."""
    tool = WebFetchTool()
    with patch("socket.getaddrinfo", side_effect=OSError("Name or service not known")):
        result = await tool.execute({"url": "http://example.com/"})
    assert not result.success
    assert "cannot resolve host" in result.error
    assert result.error_kind == "dependency"
    assert result.is_infra_failure


# ── timeout budget ──────────────────────────────────────────────────────────


def test_request_timeout_stays_under_registry_wait_for():
    """The registry wraps execute() in wait_for(timeout_seconds); an equal inner
    budget raced it and made classification depend on who won."""
    tool = WebFetchTool()
    assert tool._request_timeout() < tool.timeout_seconds
    assert tool._request_timeout() > 0


def test_request_timeout_never_goes_non_positive():
    tool = WebFetchTool()
    tool.timeout_seconds = 2  # smaller than the margin
    assert tool._request_timeout() >= 1.0


# ── web_search ──────────────────────────────────────────────────────────────


@pytest.mark.asyncio
async def test_search_timeout_classified_and_keeps_provider_metadata():
    tool = WebSearchTool(api_key="k", provider="brave")

    async def boom(*a, **kw):
        raise asyncio.TimeoutError()

    with patch.object(WebSearchTool, "_search_brave", boom):
        result = await tool.execute({"query": "hello"})
    assert not result.success
    assert result.error_kind == "timeout"
    assert result.metadata["provider"] == "brave"


@pytest.mark.asyncio
async def test_search_missing_api_key_is_not_infra():
    tool = WebSearchTool(api_key="", provider="brave")
    result = await tool.execute({"query": "hello"})
    assert not result.success
    assert result.error_kind == ""
    assert not result.is_infra_failure
