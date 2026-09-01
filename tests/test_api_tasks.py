# tests/test_api_tasks.py
import pytest
from unittest.mock import AsyncMock, MagicMock
from aiohttp import web
from aiohttp.test_utils import TestServer, TestClient

from echo_agent.agent.loop import AgentLoop
from echo_agent.gateway.api.tasks import TasksAPI


@pytest.fixture
def mock_server():
    server = MagicMock()
    server._require_api_token = MagicMock(return_value=None)
    # Write endpoints moved to _write_guard (which delegates to
    # _require_admin_token). Mirror the read-level stub for the write-level
    # one so these tests exercise handler logic, not auth wiring.
    server._require_admin_token = MagicMock(return_value=None)
    # spec_set=AgentLoop so assigning an attribute the loop does not expose
    # raises AttributeError here — catching contract drift the API would hit.
    server._agent_loop = MagicMock(spec_set=AgentLoop)
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
        resp = await client.post(
            "/api/v1/tasks",
            json={
                "title": "new task",
                "parent_task_id": "parent-1",
                "metadata": {"priority": "high"},
            },
        )
        assert resp.status == 201
        data = await resp.json()
        assert data["task"]["title"] == "new task"
        assert mock_server._agent_loop.task_manager.create.await_args.kwargs["parent_task_id"] == "parent-1"
        assert mock_server._agent_loop.task_manager.create.await_args.kwargs["metadata"] == {"priority": "high"}


@pytest.mark.asyncio
async def test_create_task_rejects_non_object_metadata(mock_server, api):
    app = web.Application()
    app.router.add_post("/api/v1/tasks", api.create_task)
    async with TestClient(TestServer(app)) as client:
        resp = await client.post("/api/v1/tasks", json={"title": "new task", "metadata": []})
        assert resp.status == 400
        assert (await resp.json())["error"] == "metadata must be an object"
        mock_server._agent_loop.task_manager.create.assert_not_awaited()


@pytest.mark.parametrize("method,path,payload", [
    ("post", "/api/v1/tasks", []),
    ("patch", "/api/v1/tasks/t1", []),
    ("patch", "/api/v1/tasks/t1", "title"),
])
@pytest.mark.asyncio
async def test_task_writes_reject_non_object_json(
    mock_server, api, method, path, payload,
):
    app = web.Application()
    app.router.add_post("/api/v1/tasks", api.create_task)
    app.router.add_patch("/api/v1/tasks/{id}", api.update_task)
    async with TestClient(TestServer(app)) as client:
        resp = await getattr(client, method)(path, json=payload)
        assert resp.status == 400
        assert (await resp.json())["error"] == "JSON body must be an object"

    mock_server._agent_loop.task_manager.create.assert_not_awaited()
    mock_server._agent_loop.task_manager.update.assert_not_awaited()


@pytest.mark.asyncio
async def test_task_create_rejects_json_null(mock_server, api):
    app = web.Application()
    app.router.add_post("/api/v1/tasks", api.create_task)
    async with TestClient(TestServer(app)) as client:
        resp = await client.post(
            "/api/v1/tasks", data="null", headers={"Content-Type": "application/json"},
        )
        assert resp.status == 400
        assert (await resp.json())["error"] == "JSON body must be an object"
    mock_server._agent_loop.task_manager.create.assert_not_awaited()


# TaskRecord 是普通 dataclass,不做任何类型收敛,所以非法值会直接落盘并被原样读回。
# 看板前端把 priority 当 number、labels 当数组用(web/src/stores/kanban.ts),
# 因此这里不校验就等于让 API 写坏自己的数据契约。
@pytest.mark.parametrize("payload,expected", [
    ({"priority": "high"}, "priority must be an integer"),
    ({"priority": None}, "priority must be an integer"),
    # bool 是 int 的子类,True 当优先级是调用方 bug,不能当成 5 静默接受。
    ({"priority": True}, "priority must be an integer"),
    ({"labels": "weekly"}, "labels must be an array of strings"),
    ({"labels": [1, 2]}, "labels must be an array of strings"),
    ({"metadata": []}, "metadata must be an object"),
])
@pytest.mark.asyncio
async def test_create_task_rejects_malformed_fields(mock_server, api, payload, expected):
    app = web.Application()
    app.router.add_post("/api/v1/tasks", api.create_task)
    async with TestClient(TestServer(app)) as client:
        resp = await client.post("/api/v1/tasks", json={"title": "t", **payload})
        assert resp.status == 400
        assert (await resp.json())["error"] == expected
        mock_server._agent_loop.task_manager.create.assert_not_awaited()


@pytest.mark.parametrize("payload,expected", [
    ({"priority": "high"}, "priority must be an integer"),
    ({"labels": "weekly"}, "labels must be an array of strings"),
    ({"metadata": []}, "metadata must be an object"),
])
@pytest.mark.asyncio
async def test_update_task_rejects_malformed_fields(mock_server, api, payload, expected):
    # PATCH 走的是另一条路径:只修 create 会留下同一个数据损坏入口。
    app = web.Application()
    app.router.add_patch("/api/v1/tasks/{id}", api.update_task)
    async with TestClient(TestServer(app)) as client:
        resp = await client.patch("/api/v1/tasks/t1", json=payload)
        assert resp.status == 400
        assert (await resp.json())["error"] == expected
        mock_server._agent_loop.task_manager.update.assert_not_awaited()


@pytest.mark.asyncio
async def test_update_task_only_checks_present_keys(mock_server, api):
    # PATCH 语义:没传的字段不能因为"缺失"被判非法。
    from echo_agent.tasks.models import TaskRecord

    mock_server._agent_loop.task_manager.update.return_value = TaskRecord(title="t")
    app = web.Application()
    app.router.add_patch("/api/v1/tasks/{id}", api.update_task)
    async with TestClient(TestServer(app)) as client:
        resp = await client.patch("/api/v1/tasks/t1", json={"title": "renamed"})
        assert resp.status == 200


@pytest.mark.asyncio
async def test_transition_task(mock_server, api):
    from echo_agent.tasks.models import TaskRecord, TaskStatus
    mock_server._agent_loop.task_manager.get = AsyncMock(
        return_value=TaskRecord(title="t", status=TaskStatus.PENDING)
    )
    mock_server._agent_loop.task_manager.transition = AsyncMock(
        return_value=TaskRecord(title="t", status=TaskStatus.QUEUED)
    )
    mock_server._agent_loop.workflow_engine = None

    app = web.Application()
    app.router.add_post("/api/v1/tasks/{id}/transition", api.transition_task)
    async with TestClient(TestServer(app)) as client:
        resp = await client.post("/api/v1/tasks/t_123/transition", json={"to": "queued"})
        assert resp.status == 200


@pytest.mark.asyncio
async def test_transition_invalid_returns_400(mock_server, api):
    from echo_agent.tasks.models import TaskRecord, TaskStatus
    mock_server._agent_loop.task_manager.get = AsyncMock(
        return_value=TaskRecord(title="t", status=TaskStatus.PENDING)
    )
    mock_server._agent_loop.task_manager.transition = AsyncMock(
        side_effect=ValueError("Invalid transition: pending → success")
    )

    app = web.Application()
    app.router.add_post("/api/v1/tasks/{id}/transition", api.transition_task)
    async with TestClient(TestServer(app)) as client:
        resp = await client.post("/api/v1/tasks/t_123/transition", json={"to": "success"})
        assert resp.status == 400


@pytest.mark.asyncio
async def test_transition_running_to_cancelled_interrupts_turn(mock_server, api):
    """The Kanban cancels via the generic transition endpoint, not DELETE. Moving a
    RUNNING task off "running" must also stop the executing turn cooperatively —
    otherwise the board says cancelled while the agent keeps running."""
    from echo_agent.tasks.models import TaskRecord, TaskStatus
    running = TaskRecord(
        id="t_run", title="running", status=TaskStatus.RUNNING,
        session_id="task:t_run", metadata={"_interrupt_event_id": "evt_9"},
    )
    cancelled = TaskRecord(id="t_run", title="running", status=TaskStatus.CANCELLED)
    mock_server._agent_loop.task_manager.get = AsyncMock(return_value=running)
    mock_server._agent_loop.task_manager.transition = AsyncMock(return_value=cancelled)
    mock_server._agent_loop.workflow_engine = None
    mock_server._bus = MagicMock()
    mock_server._bus.publish_inbound = AsyncMock(return_value=True)

    app = web.Application()
    app.router.add_post("/api/v1/tasks/{id}/transition", api.transition_task)
    async with TestClient(TestServer(app)) as client:
        resp = await client.post("/api/v1/tasks/t_run/transition", json={"to": "cancelled"})
        assert resp.status == 200

    mock_server._bus.publish_inbound.assert_awaited_once()
    event = mock_server._bus.publish_inbound.await_args.args[0]
    assert event.text == "/__interrupt__"
    assert event.session_key == "task:t_run"
    assert event.metadata["_interrupt_target_event_id"] == "evt_9"


@pytest.mark.asyncio
async def test_transition_non_running_skips_interrupt(mock_server, api):
    """A pending→queued board move isn't stopping an executing turn, so no interrupt."""
    from echo_agent.tasks.models import TaskRecord, TaskStatus
    pending = TaskRecord(id="t_p", title="p", status=TaskStatus.PENDING)
    queued = TaskRecord(id="t_p", title="p", status=TaskStatus.QUEUED)
    mock_server._agent_loop.task_manager.get = AsyncMock(return_value=pending)
    mock_server._agent_loop.task_manager.transition = AsyncMock(return_value=queued)
    mock_server._agent_loop.workflow_engine = None
    mock_server._bus = MagicMock()
    mock_server._bus.publish_inbound = AsyncMock(return_value=True)

    app = web.Application()
    app.router.add_post("/api/v1/tasks/{id}/transition", api.transition_task)
    async with TestClient(TestServer(app)) as client:
        resp = await client.post("/api/v1/tasks/t_p/transition", json={"to": "queued"})
        assert resp.status == 200

    mock_server._bus.publish_inbound.assert_not_awaited()


@pytest.mark.asyncio
async def test_cancel_running_task_interrupts_its_turn(mock_server, api):
    """Cancelling a RUNNING task must also publish a scoped /__interrupt__ control
    event so the executing turn stops cooperatively — not just flip the DB status."""
    from echo_agent.tasks.models import TaskRecord, TaskStatus
    running = TaskRecord(
        id="t_run", title="running", status=TaskStatus.RUNNING,
        session_id="task:t_run", metadata={"_interrupt_event_id": "evt_42"},
    )
    mock_server._agent_loop.task_manager.get = AsyncMock(return_value=running)
    mock_server._agent_loop.task_manager.cancel = AsyncMock(return_value=running)
    mock_server._bus = MagicMock()
    mock_server._bus.publish_inbound = AsyncMock(return_value=True)

    app = web.Application()
    app.router.add_delete("/api/v1/tasks/{id}", api.delete_task)
    async with TestClient(TestServer(app)) as client:
        resp = await client.delete("/api/v1/tasks/t_run")
        assert resp.status == 200

    mock_server._agent_loop.task_manager.cancel.assert_awaited_once_with("t_run")
    mock_server._bus.publish_inbound.assert_awaited_once()
    event = mock_server._bus.publish_inbound.await_args.args[0]
    assert event.text == "/__interrupt__"
    assert event.is_control is True
    assert event.session_key == "task:t_run"
    assert event.metadata["_interrupt_target_event_id"] == "evt_42"


@pytest.mark.asyncio
async def test_cancel_non_running_task_skips_interrupt(mock_server, api):
    """A queued/pending task isn't executing, so cancel should not fire an interrupt."""
    from echo_agent.tasks.models import TaskRecord, TaskStatus
    pending = TaskRecord(id="t_pend", title="pending", status=TaskStatus.PENDING)
    mock_server._agent_loop.task_manager.get = AsyncMock(return_value=pending)
    mock_server._agent_loop.task_manager.cancel = AsyncMock(return_value=pending)
    mock_server._bus = MagicMock()
    mock_server._bus.publish_inbound = AsyncMock(return_value=True)

    app = web.Application()
    app.router.add_delete("/api/v1/tasks/{id}", api.delete_task)
    async with TestClient(TestServer(app)) as client:
        resp = await client.delete("/api/v1/tasks/t_pend")
        assert resp.status == 200

    mock_server._bus.publish_inbound.assert_not_awaited()


@pytest.mark.asyncio
async def test_transition_to_running_rejected(mock_server, api):
    """Entering running is the dispatcher's job (it also attaches the executor).
    A manual transition to running would create an orphan running task, so the
    endpoint must reject it with 400 and never touch the manager."""
    from echo_agent.tasks.models import TaskRecord, TaskStatus
    mock_server._agent_loop.task_manager.get = AsyncMock(
        return_value=TaskRecord(id="t_q", title="q", status=TaskStatus.QUEUED)
    )
    mock_server._agent_loop.task_manager.transition = AsyncMock()

    app = web.Application()
    app.router.add_post("/api/v1/tasks/{id}/transition", api.transition_task)
    async with TestClient(TestServer(app)) as client:
        resp = await client.post("/api/v1/tasks/t_q/transition", json={"to": "running"})
        assert resp.status == 400

    mock_server._agent_loop.task_manager.transition.assert_not_awaited()


@pytest.mark.asyncio
async def test_retry_endpoint_calls_manager_retry(mock_server, api):
    """Retry must go through manager.retry (increments retry_count, enforces
    max_retries), not a plain failed→queued transition that bypasses both."""
    from echo_agent.tasks.models import TaskRecord, TaskStatus
    retried = TaskRecord(id="t_f", title="f", status=TaskStatus.QUEUED, retry_count=1)
    mock_server._agent_loop.task_manager.retry = AsyncMock(return_value=retried)

    app = web.Application()
    app.router.add_post("/api/v1/tasks/{id}/retry", api.retry_task)
    async with TestClient(TestServer(app)) as client:
        resp = await client.post("/api/v1/tasks/t_f/retry")
        assert resp.status == 200
        data = await resp.json()
        assert data["task"]["retry_count"] == 1

    mock_server._agent_loop.task_manager.retry.assert_awaited_once_with("t_f")


@pytest.mark.asyncio
async def test_retry_endpoint_surfaces_max_retries_error(mock_server, api):
    """When manager.retry rejects (max_retries exceeded / not failed), the
    endpoint returns 400 with the reason rather than silently succeeding."""
    mock_server._agent_loop.task_manager.retry = AsyncMock(
        side_effect=ValueError("Max retries (3) exceeded")
    )

    app = web.Application()
    app.router.add_post("/api/v1/tasks/{id}/retry", api.retry_task)
    async with TestClient(TestServer(app)) as client:
        resp = await client.post("/api/v1/tasks/t_f/retry")
        assert resp.status == 400
        data = await resp.json()
        assert "Max retries" in data["error"]
