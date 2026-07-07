# tests/test_api_tasks.py
import pytest
from unittest.mock import AsyncMock, MagicMock
from aiohttp import web
from aiohttp.test_utils import TestServer, TestClient

from echo_agent.gateway.api.tasks import TasksAPI


@pytest.fixture
def mock_server():
    server = MagicMock()
    server._require_api_token = MagicMock(return_value=None)
    server._agent_loop = MagicMock()
    server._agent_loop.task_manager = AsyncMock()
    return server


@pytest.fixture
def api(mock_server):
    return TasksAPI(mock_server)


@pytest.mark.asyncio
async def test_list_tasks_returns_json(mock_server, api):
    from echo_agent.tasks.models import TaskRecord, TaskStatus
    task = TaskRecord(title="test task", status=TaskStatus.PENDING, board_id="default")
    mock_server._agent_loop.task_manager.list_by_filters = AsyncMock(return_value=[task])

    app = web.Application()
    app.router.add_get("/api/v1/tasks", api.list_tasks)
    async with TestClient(TestServer(app)) as client:
        resp = await client.get("/api/v1/tasks")
        assert resp.status == 200
        data = await resp.json()
        assert len(data["tasks"]) == 1
        assert data["tasks"][0]["title"] == "test task"


@pytest.mark.asyncio
async def test_create_task(mock_server, api):
    from echo_agent.tasks.models import TaskRecord
    mock_server._agent_loop.task_manager.create = AsyncMock(
        return_value=TaskRecord(title="new task")
    )

    app = web.Application()
    app.router.add_post("/api/v1/tasks", api.create_task)
    async with TestClient(TestServer(app)) as client:
        resp = await client.post("/api/v1/tasks", json={"title": "new task"})
        assert resp.status == 201
        data = await resp.json()
        assert data["task"]["title"] == "new task"


@pytest.mark.asyncio
async def test_transition_task(mock_server, api):
    from echo_agent.tasks.models import TaskRecord, TaskStatus
    mock_server._agent_loop.task_manager.transition = AsyncMock(
        return_value=TaskRecord(title="t", status=TaskStatus.QUEUED)
    )

    app = web.Application()
    app.router.add_post("/api/v1/tasks/{id}/transition", api.transition_task)
    async with TestClient(TestServer(app)) as client:
        resp = await client.post("/api/v1/tasks/t_123/transition", json={"to": "queued"})
        assert resp.status == 200


@pytest.mark.asyncio
async def test_transition_invalid_returns_400(mock_server, api):
    mock_server._agent_loop.task_manager.transition = AsyncMock(
        side_effect=ValueError("Invalid transition: pending → success")
    )

    app = web.Application()
    app.router.add_post("/api/v1/tasks/{id}/transition", api.transition_task)
    async with TestClient(TestServer(app)) as client:
        resp = await client.post("/api/v1/tasks/t_123/transition", json={"to": "success"})
        assert resp.status == 400
