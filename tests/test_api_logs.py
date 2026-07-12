# tests/test_api_logs.py
import pytest
from unittest.mock import MagicMock
from aiohttp import web
from aiohttp.test_utils import TestServer, TestClient

from echo_agent.agent.loop import AgentLoop
from echo_agent.gateway.api.logs import LogsAPI


@pytest.fixture
def mock_server():
    server = MagicMock()
    server._require_api_token = MagicMock(return_value=None)
    # spec_set=AgentLoop so assigning an attribute the loop does not expose
    # raises AttributeError here — catching contract drift the API would hit.
    server._agent_loop = MagicMock(spec_set=AgentLoop)
    server._agent_loop.log_buffer = [
        {"ts": "2026-07-07T10:00:00", "level": "INFO", "message": "Started"},
        {"ts": "2026-07-07T10:00:01", "level": "ERROR", "message": "Oops"},
    ]
    return server


@pytest.fixture
def api(mock_server):
    return LogsAPI(mock_server)


@pytest.mark.asyncio
async def test_list_logs(mock_server, api):
    app = web.Application()
    app.router.add_get("/api/v1/logs", api.list_logs)
    async with TestClient(TestServer(app)) as client:
        resp = await client.get("/api/v1/logs")
        assert resp.status == 200
        data = await resp.json()
        assert len(data["logs"]) == 2


@pytest.mark.asyncio
async def test_list_logs_filter_level(mock_server, api):
    app = web.Application()
    app.router.add_get("/api/v1/logs", api.list_logs)
    async with TestClient(TestServer(app)) as client:
        resp = await client.get("/api/v1/logs?level=ERROR")
        assert resp.status == 200
        data = await resp.json()
        assert len(data["logs"]) == 1
        assert data["logs"][0]["level"] == "ERROR"
