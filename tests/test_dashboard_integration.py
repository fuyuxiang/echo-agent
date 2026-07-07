# tests/test_dashboard_integration.py
import pytest
from unittest.mock import MagicMock, AsyncMock
from aiohttp import web
from aiohttp.test_utils import TestServer, TestClient

from echo_agent.gateway.api.tasks import TasksAPI
from echo_agent.gateway.api.sessions import SessionsAPI
from echo_agent.gateway.api.cron_api import CronAPI
from echo_agent.gateway.api.logs import LogsAPI
from echo_agent.gateway.api.analytics import AnalyticsAPI
from echo_agent.tasks.models import TaskStatus, TaskRecord, VALID_TASK_TRANSITIONS


class TestTaskStateMachine:
    def test_all_transitions_valid(self):
        for status in TaskStatus:
            assert status in VALID_TASK_TRANSITIONS

    def test_blocked_and_review_exist(self):
        assert TaskStatus.BLOCKED.value == "blocked"
        assert TaskStatus.REVIEW.value == "review"

    def test_no_transition_out_of_terminal(self):
        assert VALID_TASK_TRANSITIONS[TaskStatus.SUCCESS] == set()
        assert VALID_TASK_TRANSITIONS[TaskStatus.CANCELLED] == set()

    def test_task_record_roundtrip(self):
        task = TaskRecord(
            title="integration test",
            labels=["test"],
            assignee="agent-x",
            source="human",
            board_id="default",
            status=TaskStatus.REVIEW,
            review_summary="looks good",
        )
        d = task.to_dict()
        restored = TaskRecord.from_dict(d)
        assert restored.status == TaskStatus.REVIEW
        assert restored.labels == ["test"]
        assert restored.review_summary == "looks good"
        assert restored.board_id == "default"


@pytest.mark.asyncio
async def test_full_task_lifecycle():
    server = MagicMock()
    server._require_api_token = MagicMock(return_value=None)

    tasks_store: dict[str, TaskRecord] = {}

    async def mock_create(**kwargs):
        task = TaskRecord(**kwargs)
        tasks_store[task.id] = task
        return task

    async def mock_transition(task_id, new_status):
        task = tasks_store[task_id]
        task.status = new_status
        return task

    async def mock_list(**kwargs):
        return list(tasks_store.values())

    manager = AsyncMock()
    manager.create = mock_create
    manager.transition = mock_transition
    manager.list_by_filters = mock_list
    server._agent_loop.task_manager = manager

    api = TasksAPI(server)
    app = web.Application()
    app.router.add_post("/api/v1/tasks", api.create_task)
    app.router.add_post("/api/v1/tasks/{id}/transition", api.transition_task)
    app.router.add_get("/api/v1/tasks", api.list_tasks)

    async with TestClient(TestServer(app)) as client:
        resp = await client.post("/api/v1/tasks", json={"title": "test task", "source": "human"})
        assert resp.status == 201
        data = await resp.json()
        task_id = data["task"]["id"]

        resp = await client.post(f"/api/v1/tasks/{task_id}/transition", json={"to": "queued"})
        assert resp.status == 200

        resp = await client.get("/api/v1/tasks")
        assert resp.status == 200
        data = await resp.json()
        assert data["tasks"][0]["status"] == "queued"
