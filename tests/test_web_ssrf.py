"""M4-1 SSRF hardening regressions.

Pins the hardened behaviour: private/loopback targets blocked, DNS-validated
IPs returned for pinning, redirects re-validated per hop (no unchecked 30x into
internal targets), proxy no longer bypasses validation, and web_search's
configurable api_base is validated.
"""

from __future__ import annotations

from unittest.mock import AsyncMock, MagicMock, patch

import pytest

from echo_agent.agent.tools.web import (
    WebFetchTool, WebSearchTool, check_url_ssrf, resolve_and_validate,
)


def _addrinfo(*ips: str):
    import socket
    return [(socket.AF_INET, socket.SOCK_STREAM, 6, "", (ip, 0)) for ip in ips]


# ── validation primitives ────────────────────────────────────────────────────


@pytest.mark.asyncio
async def test_resolve_and_validate_blocks_private():
    with patch("socket.getaddrinfo", return_value=_addrinfo("127.0.0.1")):
        ips, error = await resolve_and_validate("http://localhost.test/")
    assert ips == []
    assert error is not None and "non-public" in error


@pytest.mark.asyncio
async def test_resolve_and_validate_blocks_cloud_metadata():
    with patch("socket.getaddrinfo", return_value=_addrinfo("169.254.169.254")):
        _, error = await resolve_and_validate("http://metadata.test/latest/meta-data")
    assert error is not None


@pytest.mark.asyncio
async def test_resolve_and_validate_allows_public_and_returns_ips():
    with patch("socket.getaddrinfo", return_value=_addrinfo("93.184.216.34")):
        ips, error = await resolve_and_validate("https://example.com/")
    assert error is None
    assert ips == ["93.184.216.34"]


@pytest.mark.asyncio
async def test_check_url_ssrf_rejects_bad_scheme():
    assert await check_url_ssrf("file:///etc/passwd") is not None


@pytest.mark.asyncio
async def test_integer_ip_is_blocked():
    # 2130706433 == 127.0.0.1; getaddrinfo resolves it to loopback.
    with patch("socket.getaddrinfo", return_value=_addrinfo("127.0.0.1")):
        _, error = await resolve_and_validate("http://2130706433/")
    assert error is not None


# ── web_fetch SSRF path ──────────────────────────────────────────────────────


@pytest.mark.asyncio
async def test_fetch_blocks_private_target():
    tool = WebFetchTool()  # allow_private defaults False
    with patch("socket.getaddrinfo", return_value=_addrinfo("10.0.0.5")):
        result = await tool.execute({"url": "http://internal.test/"}, None)
    assert result.success is False
    assert "non-public" in result.error


@pytest.mark.asyncio
async def test_fetch_blocks_private_even_with_proxy():
    # Previously a configured proxy skipped SSRF entirely. Now it still validates.
    tool = WebFetchTool(proxy="http://proxy.local:8080")
    with patch("socket.getaddrinfo", return_value=_addrinfo("10.0.0.5")):
        result = await tool.execute({"url": "http://internal.test/"}, None)
    assert result.success is False
    assert "non-public" in result.error


@pytest.mark.asyncio
async def test_redirect_to_private_is_revalidated_and_blocked():
    """A public URL that 302-redirects to an internal target must be blocked
    on the second hop, not followed."""
    tool = WebFetchTool()

    # First hop: public IP, returns a 302 to an internal URL.
    redirect_resp = MagicMock()
    redirect_resp.status = 302
    redirect_resp.headers = {"Location": "http://internal.test/secret"}
    redirect_resp.__aenter__ = AsyncMock(return_value=redirect_resp)
    redirect_resp.__aexit__ = AsyncMock(return_value=False)

    session = MagicMock()
    session.get = MagicMock(return_value=redirect_resp)
    session.__aenter__ = AsyncMock(return_value=session)
    session.__aexit__ = AsyncMock(return_value=False)

    def fake_getaddrinfo(host, *a, **k):
        return _addrinfo("93.184.216.34") if host == "example.com" else _addrinfo("10.0.0.5")

    with patch("socket.getaddrinfo", side_effect=fake_getaddrinfo), \
         patch("aiohttp.ClientSession", return_value=session), \
         patch("aiohttp.TCPConnector", return_value=MagicMock()):
        result = await tool.execute({"url": "https://example.com/start"}, None)

    assert result.success is False
    assert "non-public" in result.error


# ── web_search api_base validation ───────────────────────────────────────────


@pytest.mark.asyncio
async def test_web_search_blocks_internal_api_base():
    tool = WebSearchTool(provider="searxng", api_base="http://internal.test/")
    with patch("socket.getaddrinfo", return_value=_addrinfo("10.0.0.5")):
        result = await tool.execute({"query": "hi"}, None)
    assert result.success is False
    assert "non-public" in result.error
