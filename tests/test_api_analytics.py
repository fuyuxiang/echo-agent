# tests/test_api_analytics.py
import pytest
from unittest.mock import MagicMock, AsyncMock
from aiohttp import web
from aiohttp.test_utils import TestServer, TestClient

from echo_agent.agent.loop import AgentLoop
from echo_agent.gateway.api.analytics import AnalyticsAPI


@pytest.fixture
def mock_server():
    server = MagicMock()
    server._require_api_token = MagicMock(return_value=None)
    # spec_set=AgentLoop so assigning an attribute the loop does not expose
    # raises AttributeError here — catching contract drift the API would hit.
    server._agent_loop = MagicMock(spec_set=AgentLoop)
    server._agent_loop.cost_tracker = MagicMock()
    server._agent_loop.cost_tracker.get_daily_usage = AsyncMock(return_value=[
        {"date": "2026-07-07", "model": "gpt-4o", "input_tokens": 1000, "output_tokens": 500, "cost_usd": 0.05}
    ])
    server._agent_loop.cost_tracker.get_skill_usage = AsyncMock(return_value=[
        {"skill": "web_search", "count": 42}
    ])
    server._agent_loop.cost_tracker.get_channel_usage = AsyncMock(return_value=[
        {"channel": "telegram", "messages": 100}
    ])
    return server


@pytest.fixture
def api(mock_server):
    return AnalyticsAPI(mock_server)


@pytest.mark.asyncio
async def test_token_analytics(mock_server, api):
    app = web.Application()
    app.router.add_get("/api/v1/analytics/tokens", api.token_usage)
    async with TestClient(TestServer(app)) as client:
        resp = await client.get("/api/v1/analytics/tokens?days=7")
        assert resp.status == 200
        data = await resp.json()
        assert len(data["usage"]) == 1


@pytest.mark.asyncio
async def test_skill_analytics(mock_server, api):
    app = web.Application()
    app.router.add_get("/api/v1/analytics/skills", api.skill_usage)
    async with TestClient(TestServer(app)) as client:
        resp = await client.get("/api/v1/analytics/skills")
        assert resp.status == 200
        data = await resp.json()
        assert data["skills"][0]["skill"] == "web_search"


@pytest.mark.asyncio
async def test_channel_analytics(mock_server, api):
    app = web.Application()
    app.router.add_get("/api/v1/analytics/channels", api.channel_usage)
    async with TestClient(TestServer(app)) as client:
        resp = await client.get("/api/v1/analytics/channels")
        assert resp.status == 200
        data = await resp.json()
        assert data["channels"][0]["channel"] == "telegram"
