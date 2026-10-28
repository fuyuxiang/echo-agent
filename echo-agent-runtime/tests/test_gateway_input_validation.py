"""Reject malformed management requests before storage or tool execution."""

import json
from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest

from echo_agent.gateway.api.cron_api import CronAPI
from echo_agent.gateway.api.memory import MemoryAPI
from echo_agent.gateway.api.skills import SkillsAPI
from echo_agent.gateway.api.tasks import TasksAPI
from aiohttp import web


def api_request(body):
    return SimpleNamespace(json=AsyncMock(return_value=body), match_info={"id": "test"}, headers={})


def server():
    value = MagicMock()
    value._require_api_token.return_value = None
    value._require_admin_token.return_value = None
    value._agent_loop.config.memory.enabled = True
    value._agent_loop.config.skills.enabled = True
    return value


@pytest.mark.asyncio
@pytest.mark.parametrize("body", [[], None, "bad", 42, True])
@pytest.mark.parametrize("api,method", [
    (CronAPI, "create_job"), (CronAPI, "update_job"),
    (MemoryAPI, "search"), (MemoryAPI, "update_entry"),
    (SkillsAPI, "import_skill"), (TasksAPI, "transition_task"),
])
async def test_management_endpoints_reject_non_object_json(api, method, body):
    response = await getattr(api(server()), method)(api_request(body))
    assert response.status == 400
    assert "error" in json.loads(response.body)


@pytest.mark.asyncio
@pytest.mark.parametrize("fields", [
    {"title": []}, {"title": "   "}, {"description": {}},
    {"assignee": 12}, {"board_id": []}, {"parent_task_id": False},
    {"source": {}},
])
@pytest.mark.parametrize("method", ["create_task", "update_task"])
async def test_tasks_reject_malformed_text_without_writing(method, fields):
    target = server()
    response = await getattr(TasksAPI(target), method)(api_request({"title": "QA task", **fields}))
    assert response.status == 400
    target._agent_loop.task_manager.create.assert_not_called()
    target._agent_loop.task_manager.update.assert_not_called()


@pytest.mark.asyncio
@pytest.mark.parametrize("fields", [
    {"name": []}, {"cron_expr": 123}, {"enabled": "false"},
    {"authorize_unattended": "true"}, {"payload": {"message": []}},
    {"payload": {"message": 123}},
])
@pytest.mark.parametrize("method", ["create_job", "update_job"])
async def test_cron_rejects_malformed_fields_before_scheduler_mutation(method, fields):
    target = server()
    body = {"name": "QA cron", "cron_expr": "0 0 * * *", "payload": {"message": "hello"}, **fields}
    response = await getattr(CronAPI(target), method)(api_request(body))
    assert response.status == 400
    target._agent_loop.scheduler.add_job.assert_not_called()
    target._agent_loop.scheduler.update_job.assert_not_called()


@pytest.mark.asyncio
@pytest.mark.parametrize("fields", [
    {"query": []}, {"query": "   "}, {"type": "invalid"},
    {"type": []}, {"limit": "5"}, {"limit": True}, {"limit": 0},
    {"limit": 501}, {"session_key": []},
])
async def test_memory_search_rejects_invalid_parameters_before_search(fields):
    target = server()
    response = await MemoryAPI(target).search(api_request({"query": "QA", "all_scopes": True, **fields}))
    assert response.status == 400
    target._agent_loop.memory.search_scored.assert_not_called()


@pytest.mark.asyncio
@pytest.mark.parametrize("path", [[], {}, True, 123, "   "])
async def test_skill_import_rejects_invalid_path(path):
    response = await SkillsAPI(server()).import_skill(api_request({"path": path}))
    assert response.status == 400


@pytest.mark.asyncio
@pytest.mark.parametrize("method", [
    "list_jobs", "create_job", "update_job", "delete_job", "trigger_job", "get_runs",
])
@pytest.mark.parametrize("authenticated", [True, False])
async def test_disabled_scheduler_is_reported_after_authentication(method, authenticated):
    target = server()
    target._agent_loop.scheduler = None
    if not authenticated:
        target._require_api_token.return_value = web.json_response({"error": "unauthorized"}, status=401)
        target._require_admin_token.return_value = web.json_response({"error": "unauthorized"}, status=401)
    response = await getattr(CronAPI(target), method)(api_request({}))
    assert response.status == (503 if authenticated else 401)
    if authenticated:
        assert "scheduler.enabled=false" in json.loads(response.body)["error"]
