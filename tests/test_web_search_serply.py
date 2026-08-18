"""web_search serply provider: result mapping and the required headers.

Serply sits behind Cloudflare, which rejects the default aiohttp User-Agent
(HTTP 403, error 1010), so the provider must send an explicit User-Agent along
with the X-Api-Key. These pin the response mapping to the standard
title/url/snippet shape and assert both headers are sent.
"""

from __future__ import annotations

from unittest.mock import AsyncMock, patch

import pytest

from echo_agent.agent.tools.web import WebSearchTool


class _FakeResp:
    async def __aenter__(self):
        return self

    async def __aexit__(self, *exc):
        return False


class _FakeSession:
    """Records the get() call and returns a dummy response context manager."""

    def __init__(self):
        self.calls = []

    def get(self, url, **kwargs):
        self.calls.append((url, kwargs))
        return _FakeResp()


@pytest.mark.asyncio
async def test_serply_maps_results_to_title_url_snippet():
    tool = WebSearchTool(api_key="secret", provider="serply")
    session = _FakeSession()
    payload = {
        "results": [
            {"title": "T1", "link": "https://a.example", "description": "D1"},
            {"title": "T2", "link": "https://b.example", "description": "D2"},
        ]
    }
    with patch.object(WebSearchTool, "_read_json", new=AsyncMock(return_value=payload)):
        results = await tool._search_serply(session, "hello world", 5)

    assert results == [
        {"title": "T1", "url": "https://a.example", "snippet": "D1"},
        {"title": "T2", "url": "https://b.example", "snippet": "D2"},
    ]


@pytest.mark.asyncio
async def test_serply_sends_api_key_and_user_agent():
    tool = WebSearchTool(api_key="secret", provider="serply")
    session = _FakeSession()
    with patch.object(WebSearchTool, "_read_json", new=AsyncMock(return_value={})):
        out = await tool._search_serply(session, "hello world", 5)

    assert out == []  # missing "results" degrades to an empty list, never None
    url, kwargs = session.calls[0]
    assert url.startswith("https://api.serply.io/v1/search/")
    headers = kwargs["headers"]
    assert headers["X-Api-Key"] == "secret"
    assert headers["User-Agent"]  # explicit UA is required past Cloudflare
