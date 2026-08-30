# tests/test_api_analytics.py
import pytest
from unittest.mock import MagicMock, AsyncMock
from aiohttp import web
from aiohttp.test_utils import TestServer, TestClient

from echo_agent.agent.loop import AgentLoop
from echo_agent.cost.budget import CostTracker
from echo_agent.gateway.api.analytics import AnalyticsAPI


@pytest.fixture
def mock_server():
    server = MagicMock()
    server._require_api_token = MagicMock(return_value=None)
    # spec_set=AgentLoop so assigning an attribute the loop does not expose
    # raises AttributeError here — catching contract drift the API would hit.
    server._agent_loop = MagicMock(spec_set=AgentLoop)
    # spec=CostTracker:只允许 mock 真实存在的方法。此前用裸 MagicMock() 能凭空造出
    # get_daily_usage 等不存在的方法,让测试全绿却掩盖了 CostTracker 根本没实现这些
    # 方法的事实(线上 500)。加 spec 后,方法一旦被删就在此处 AttributeError。
    server._agent_loop.cost_tracker = MagicMock(spec=CostTracker)
    server._agent_loop.cost_tracker.get_daily_usage = AsyncMock(return_value=[
        {"date": "2026-07-07", "input_tokens": 1000, "output_tokens": 500, "cost_usd": 0.05}
    ])
    server._agent_loop.cost_tracker.get_skill_usage = AsyncMock(return_value=[
        {"skill": "web_search", "count": 42}
    ])
    server._agent_loop.cost_tracker.get_channel_usage = AsyncMock(return_value=[
        {"channel": "telegram", "input_tokens": 100, "output_tokens": 50, "cost_usd": 0.01}
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


@pytest.mark.asyncio
async def test_skill_analytics_reports_unavailable_with_real_tracker():
    """无持久化存储的 CostTracker 必须如实告知技能维度不可用。

    技能埋点已实现，但 storage=None 时无法保存或聚合。客户端若只看
    skills 字段，会把"无存储"误读成"这几天没调过技能"。
    """
    server = MagicMock()
    server._require_api_token = MagicMock(return_value=None)
    server._agent_loop = MagicMock(spec_set=AgentLoop)
    server._agent_loop.cost_tracker = CostTracker(storage=None, enabled=False)
    real_api = AnalyticsAPI(server)

    app = web.Application()
    app.router.add_get("/api/v1/analytics/skills", real_api.skill_usage)
    async with TestClient(TestServer(app)) as client:
        resp = await client.get("/api/v1/analytics/skills")
        assert resp.status == 200
        data = await resp.json()
        assert data["skills"] == []
        assert data["available"] is False
        assert "storage backend" in data["unavailable_reason"]


@pytest.mark.asyncio
async def test_skill_analytics_available_when_tracker_declares_support(mock_server, api):
    """tracker 声明支持时不得输出 unavailable_reason,避免误报未实现。"""
    mock_server._agent_loop.cost_tracker.skill_usage_available = True
    app = web.Application()
    app.router.add_get("/api/v1/analytics/skills", api.skill_usage)
    async with TestClient(TestServer(app)) as client:
        resp = await client.get("/api/v1/analytics/skills")
        data = await resp.json()
        assert data["available"] is True
        assert "unavailable_reason" not in data
