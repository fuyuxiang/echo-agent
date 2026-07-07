# tests/test_api_cron.py
import pytest
from unittest.mock import AsyncMock, MagicMock
from aiohttp import web
from aiohttp.test_utils import TestServer, TestClient

from echo_agent.gateway.api.cron_api import CronAPI


@pytest.fixture
def mock_server():
    server = MagicMock()
    server._require_api_token = MagicMock(return_value=None)
    server._agent_loop = MagicMock()
    server._agent_loop.scheduler = MagicMock()
    return server


@pytest.fixture
def api(mock_server):
    return CronAPI(mock_server)


@pytest.mark.asyncio
async def test_list_cron_jobs(mock_server, api):
    from echo_agent.scheduler.service import ScheduledJob, TriggerKind
    job = ScheduledJob(id="j1", name="daily_check", trigger=TriggerKind.CRON, cron_expr="0 9 * * *")
    mock_server._agent_loop.scheduler.list_jobs = MagicMock(return_value=[job])

    app = web.Application()
    app.router.add_get("/api/v1/cron", api.list_jobs)
    async with TestClient(TestServer(app)) as client:
        resp = await client.get("/api/v1/cron")
        assert resp.status == 200
        data = await resp.json()
        assert len(data["jobs"]) == 1
        assert data["jobs"][0]["name"] == "daily_check"
        assert data["total"] == 1


@pytest.mark.asyncio
async def test_create_cron_job(mock_server, api):
    from echo_agent.scheduler.service import ScheduledJob, TriggerKind
    created_job = ScheduledJob(id="j_new", name="new_job", trigger=TriggerKind.CRON, cron_expr="*/5 * * * *")
    mock_server._agent_loop.scheduler.add_job = MagicMock(return_value=created_job)

    app = web.Application()
    app.router.add_post("/api/v1/cron", api.create_job)
    async with TestClient(TestServer(app)) as client:
        resp = await client.post("/api/v1/cron", json={
            "name": "new_job", "cron_expr": "*/5 * * * *", "payload": {"msg": "hello"}
        })
        assert resp.status == 201
        data = await resp.json()
        assert data["id"] == "j_new"


@pytest.mark.asyncio
async def test_create_cron_job_missing_expr(mock_server, api):
    app = web.Application()
    app.router.add_post("/api/v1/cron", api.create_job)
    async with TestClient(TestServer(app)) as client:
        resp = await client.post("/api/v1/cron", json={"name": "bad_job"})
        assert resp.status == 400


@pytest.mark.asyncio
async def test_delete_cron_job(mock_server, api):
    mock_server._agent_loop.scheduler.remove_job = MagicMock(return_value=True)

    app = web.Application()
    app.router.add_delete("/api/v1/cron/{id}", api.delete_job)
    async with TestClient(TestServer(app)) as client:
        resp = await client.delete("/api/v1/cron/j1")
        assert resp.status == 200
        data = await resp.json()
        assert data["status"] == "deleted"


@pytest.mark.asyncio
async def test_delete_cron_job_not_found(mock_server, api):
    mock_server._agent_loop.scheduler.remove_job = MagicMock(return_value=False)

    app = web.Application()
    app.router.add_delete("/api/v1/cron/{id}", api.delete_job)
    async with TestClient(TestServer(app)) as client:
        resp = await client.delete("/api/v1/cron/nonexistent")
        assert resp.status == 404


@pytest.mark.asyncio
async def test_trigger_cron_job(mock_server, api):
    mock_server._agent_loop.scheduler.trigger_job = AsyncMock(return_value=True)

    app = web.Application()
    app.router.add_post("/api/v1/cron/{id}/trigger", api.trigger_job)
    async with TestClient(TestServer(app)) as client:
        resp = await client.post("/api/v1/cron/j1/trigger")
        assert resp.status == 200
        data = await resp.json()
        assert data["status"] == "triggered"


@pytest.mark.asyncio
async def test_trigger_cron_job_not_found(mock_server, api):
    mock_server._agent_loop.scheduler.trigger_job = AsyncMock(return_value=False)

    app = web.Application()
    app.router.add_post("/api/v1/cron/{id}/trigger", api.trigger_job)
    async with TestClient(TestServer(app)) as client:
        resp = await client.post("/api/v1/cron/nonexistent/trigger")
        assert resp.status == 404


@pytest.mark.asyncio
async def test_update_cron_job(mock_server, api):
    from echo_agent.scheduler.service import ScheduledJob, TriggerKind
    job = ScheduledJob(id="j1", name="old_name", trigger=TriggerKind.CRON, cron_expr="0 9 * * *")
    mock_server._agent_loop.scheduler.get_job = MagicMock(return_value=job)

    app = web.Application()
    app.router.add_put("/api/v1/cron/{id}", api.update_job)
    async with TestClient(TestServer(app)) as client:
        resp = await client.put("/api/v1/cron/j1", json={"name": "new_name", "cron_expr": "0 10 * * *"})
        assert resp.status == 200
        data = await resp.json()
        assert data["job"]["name"] == "new_name"
        assert data["job"]["cron_expr"] == "0 10 * * *"


@pytest.mark.asyncio
async def test_update_cron_job_not_found(mock_server, api):
    mock_server._agent_loop.scheduler.get_job = MagicMock(return_value=None)

    app = web.Application()
    app.router.add_put("/api/v1/cron/{id}", api.update_job)
    async with TestClient(TestServer(app)) as client:
        resp = await client.put("/api/v1/cron/nonexistent", json={"name": "x"})
        assert resp.status == 404


@pytest.mark.asyncio
async def test_get_runs(mock_server, api):
    mock_server._agent_loop.scheduler.get_run_history = MagicMock(return_value=[
        {"ts": 1000, "status": "completed"},
        {"ts": 2000, "status": "error"},
    ])

    app = web.Application()
    app.router.add_get("/api/v1/cron/{id}/runs", api.get_runs)
    async with TestClient(TestServer(app)) as client:
        resp = await client.get("/api/v1/cron/j1/runs?limit=5")
        assert resp.status == 200
        data = await resp.json()
        assert len(data["runs"]) == 2
