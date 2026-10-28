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


@pytest.mark.asyncio
async def test_list_logs_newest_first(mock_server, api):
    """Page 1 must start at the most recent record, not the oldest."""
    app = web.Application()
    app.router.add_get("/api/v1/logs", api.list_logs)
    async with TestClient(TestServer(app)) as client:
        resp = await client.get("/api/v1/logs")
        data = await resp.json()
        assert [e["message"] for e in data["logs"]] == ["Oops", "Started"]


@pytest.mark.asyncio
async def test_list_logs_limit_serves_newest_window(mock_server, api):
    """The regression this guards: with a full ring buffer, slicing in append
    order returned the oldest window and the newest entry was unreachable."""
    mock_server._agent_loop.log_buffer = [
        {"ts": f"2026-07-07T10:00:{i:02d}", "level": "INFO", "message": f"m{i}"}
        for i in range(300)
    ]
    app = web.Application()
    app.router.add_get("/api/v1/logs", api.list_logs)
    async with TestClient(TestServer(app)) as client:
        resp = await client.get("/api/v1/logs?limit=200")
        data = await resp.json()
        assert data["total"] == 300
        assert len(data["logs"]) == 200
        assert data["logs"][0]["message"] == "m299"
        assert data["logs"][-1]["message"] == "m100"


@pytest.mark.asyncio
async def test_list_logs_offset_walks_backwards(mock_server, api):
    """offset pages further into the past, without overlapping page 1."""
    mock_server._agent_loop.log_buffer = [
        {"ts": f"2026-07-07T10:00:{i:02d}", "level": "INFO", "message": f"m{i}"}
        for i in range(10)
    ]
    app = web.Application()
    app.router.add_get("/api/v1/logs", api.list_logs)
    async with TestClient(TestServer(app)) as client:
        page1 = await (await client.get("/api/v1/logs?limit=4")).json()
        page2 = await (await client.get("/api/v1/logs?limit=4&offset=4")).json()
        assert [e["message"] for e in page1["logs"]] == ["m9", "m8", "m7", "m6"]
        assert [e["message"] for e in page2["logs"]] == ["m5", "m4", "m3", "m2"]


@pytest.mark.asyncio
async def test_list_logs_filter_applies_before_paging(mock_server, api):
    """total reflects the filtered set, and the newest match leads."""
    mock_server._agent_loop.log_buffer = [
        {"ts": f"2026-07-07T10:00:{i:02d}",
         "level": "ERROR" if i % 2 else "INFO",
         "message": f"m{i}"}
        for i in range(10)
    ]
    app = web.Application()
    app.router.add_get("/api/v1/logs", api.list_logs)
    async with TestClient(TestServer(app)) as client:
        data = await (await client.get("/api/v1/logs?level=ERROR&limit=2")).json()
        assert data["total"] == 5
        assert [e["message"] for e in data["logs"]] == ["m9", "m7"]

