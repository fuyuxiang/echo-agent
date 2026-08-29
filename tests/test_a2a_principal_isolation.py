"""A2A authorization is principal-based, including custom task IDs."""

from __future__ import annotations

import asyncio
import json
from unittest.mock import AsyncMock, MagicMock

import pytest
from aiohttp import web

from echo_agent.a2a.models import A2AMessage, A2ATask, AgentCard, TaskState
from echo_agent.a2a.protocol import A2AProtocol
from echo_agent.a2a.server import A2AServer
from echo_agent.config.schema import GatewayAuthConfig
from echo_agent.gateway.auth import GatewayAuth


def _send(task_id: str, text: str) -> dict:
    return {
        "jsonrpc": "2.0",
        "id": f"send-{text}",
        "method": "tasks/send",
        "params": {
            "id": task_id,
            "message": A2AMessage.text("user", text).to_dict(),
        },
    }


def _request(method: str, task_id: str, req_id: str) -> dict:
    return {
        "jsonrpc": "2.0", "id": req_id,
        "method": method, "params": {"id": task_id},
    }


@pytest.mark.asyncio
async def test_custom_ids_are_namespaced_by_principal() -> None:
    async def complete(task: A2ATask) -> A2ATask:
        task.state = TaskState.COMPLETED
        task.messages.append(A2AMessage.text("agent", task.messages[-1].text_content))
        return task

    protocol = A2AProtocol(complete)
    a = await protocol.handle(_send("shared-id", "from-a"), principal="token-a")

    foreign = await protocol.handle(
        _request("tasks/get", "shared-id", "foreign"), principal="token-b",
    )
    missing = await protocol.handle(
        _request("tasks/get", "does-not-exist", "missing"), principal="token-b",
    )
    assert foreign["error"] == missing["error"]
    assert foreign["error"] == {"code": -32001, "message": "Task not found"}

    b = await protocol.handle(_send("shared-id", "from-b"), principal="token-b")
    a_get = await protocol.handle(
        _request("tasks/get", "shared-id", "get-a"), principal="token-a",
    )
    b_get = await protocol.handle(
        _request("tasks/get", "shared-id", "get-b"), principal="token-b",
    )

    assert a["result"]["id"] == b["result"]["id"] == "shared-id"
    assert "owner" not in a["result"] and "owner" not in b["result"]
    assert [m["parts"][0]["text"] for m in a_get["result"]["messages"]] == [
        "from-a", "from-a",
    ]
    assert [m["parts"][0]["text"] for m in b_get["result"]["messages"]] == [
        "from-b", "from-b",
    ]


@pytest.mark.asyncio
async def test_foreign_cancel_is_not_found_and_does_not_stop_owner_run() -> None:
    entered = asyncio.Event()

    async def slow(task: A2ATask) -> A2ATask:
        entered.set()
        await asyncio.sleep(3600)
        return task

    protocol = A2AProtocol(slow)
    send = asyncio.create_task(
        protocol.handle(_send("running-id", "work"), principal="token-a")
    )
    await asyncio.wait_for(entered.wait(), timeout=2)

    foreign = await protocol.handle(
        _request("tasks/cancel", "running-id", "foreign"), principal="token-b",
    )
    assert foreign["error"] == {"code": -32001, "message": "Task not found"}
    assert not send.done()

    own = await protocol.handle(
        _request("tasks/cancel", "running-id", "own"), principal="token-a",
    )
    assert own["result"]["state"] == "canceled"
    with pytest.raises(asyncio.CancelledError):
        await send


@pytest.mark.asyncio
async def test_same_owner_concurrent_send_is_busy_and_cancel_tracks_only_run() -> None:
    entered = asyncio.Event()
    canceled = asyncio.Event()
    starts = 0

    async def slow(task: A2ATask) -> A2ATask:
        nonlocal starts
        starts += 1
        entered.set()
        try:
            await asyncio.sleep(3600)
        except asyncio.CancelledError:
            canceled.set()
            raise
        return task

    protocol = A2AProtocol(slow)
    first = asyncio.create_task(
        protocol.handle(_send("same-task", "first"), principal="token-a")
    )
    await asyncio.wait_for(entered.wait(), timeout=2)

    second = await protocol.handle(
        _send("same-task", "second"), principal="token-a",
    )
    assert second["error"] == {"code": -32003, "message": "Task is already running"}
    assert starts == 1
    assert len(protocol._runs) == 1

    canceled_response = await protocol.handle(
        _request("tasks/cancel", "same-task", "cancel"), principal="token-a",
    )
    assert canceled_response["result"]["state"] == "canceled"
    with pytest.raises(asyncio.CancelledError):
        await first
    assert canceled.is_set()
    assert protocol._runs == {}


@pytest.mark.asyncio
async def test_done_run_slot_remains_busy_until_owning_handler_cleans_up() -> None:
    async def complete(task: A2ATask) -> A2ATask:
        task.state = TaskState.COMPLETED
        return task

    protocol = A2AProtocol(complete)
    task = A2ATask(id="settling", owner="token-a", state=TaskState.WORKING)
    protocol._tasks.set_owned("token-a", task)
    storage_key = protocol._tasks.storage_key("token-a", task.id)
    finished_run = asyncio.create_task(complete(task))
    await finished_run
    # A completed worker can briefly remain registered while its owning
    # _handle_send is waiting to resume and commit/finally. It is still the
    # authoritative slot during that window and must not be overwritten.
    protocol._runs[storage_key] = finished_run

    response = await protocol.handle(
        _send("settling", "second"), principal="token-a",
    )

    assert response["error"] == {
        "code": -32003, "message": "Task is already running",
    }
    assert protocol._runs[storage_key] is finished_run
    protocol._runs.pop(storage_key)


@pytest.mark.asyncio
async def test_active_task_backstop_cancels_worker_without_losing_handle() -> None:
    now = [0.0]
    entered = asyncio.Event()
    canceled = asyncio.Event()

    async def slow(task: A2ATask) -> A2ATask:
        entered.set()
        try:
            await asyncio.sleep(3600)
        except asyncio.CancelledError:
            canceled.set()
            raise
        return task

    protocol = A2AProtocol(
        slow, active_task_ttl_seconds=10.0, clock=lambda: now[0],
    )
    send = asyncio.create_task(
        protocol.handle(_send("stuck", "work"), principal="token-a")
    )
    await asyncio.wait_for(entered.wait(), timeout=2)
    storage_key = protocol._tasks.storage_key("token-a", "stuck")
    run = protocol._runs[storage_key]

    now[0] = 11.0
    # Any store maintenance path activates the stuck-task backstop.
    assert protocol._tasks.get_owned("token-a", "stuck") is None
    assert protocol._runs[storage_key] is run

    with pytest.raises(asyncio.CancelledError):
        await send
    assert canceled.is_set()
    assert storage_key not in protocol._runs
    assert protocol._tasks.get_owned("token-a", "stuck").state == TaskState.CANCELED


@pytest.mark.asyncio
async def test_active_capacity_rejects_new_task_then_terminal_frees_slot() -> None:
    two_started = asyncio.Event()
    starts = 0

    async def slow(task: A2ATask) -> A2ATask:
        nonlocal starts
        starts += 1
        if starts == 2:
            two_started.set()
        await asyncio.sleep(3600)
        return task

    protocol = A2AProtocol(slow, max_tasks=2)
    first = asyncio.create_task(
        protocol.handle(_send("active-1", "first"), principal="token-a")
    )
    second = asyncio.create_task(
        protocol.handle(_send("active-2", "second"), principal="token-a")
    )
    await asyncio.wait_for(two_started.wait(), timeout=2)

    rejected = await protocol.handle(
        _send("active-3", "third"), principal="token-a",
    )
    assert rejected["error"] == {
        "code": -32004, "message": "Task capacity is exhausted",
    }
    assert starts == 2
    assert protocol._tasks.get_owned("token-a", "active-3") is None

    await protocol.handle(
        _request("tasks/cancel", "active-1", "cancel-1"), principal="token-a",
    )
    with pytest.raises(asyncio.CancelledError):
        await first

    async def complete(task: A2ATask) -> A2ATask:
        task.state = TaskState.COMPLETED
        return task

    protocol._process = complete
    admitted = await protocol.handle(
        _send("active-3", "third"), principal="token-a",
    )
    assert admitted["result"]["state"] == "completed"

    await protocol.handle(
        _request("tasks/cancel", "active-2", "cancel-2"), principal="token-a",
    )
    with pytest.raises(asyncio.CancelledError):
        await second


@pytest.mark.asyncio
async def test_capacity_counts_worker_that_swallows_backstop_cancellation() -> None:
    now = [0.0]
    entered = asyncio.Event()
    cancellation_caught = asyncio.Event()
    release = asyncio.Event()

    async def cancellation_resistant(task: A2ATask) -> A2ATask:
        entered.set()
        try:
            await asyncio.sleep(3600)
        except asyncio.CancelledError:
            cancellation_caught.set()
            await release.wait()
        task.state = TaskState.COMPLETED
        return task

    protocol = A2AProtocol(
        cancellation_resistant,
        max_tasks=1,
        active_task_ttl_seconds=10.0,
        clock=lambda: now[0],
    )
    first = asyncio.create_task(
        protocol.handle(_send("stubborn", "first"), principal="token-a")
    )
    await asyncio.wait_for(entered.wait(), timeout=2)
    now[0] = 11.0
    assert protocol._tasks.get_owned("token-a", "stubborn") is None
    await asyncio.wait_for(cancellation_caught.wait(), timeout=2)

    rejected = await protocol.handle(
        _send("new-task", "second"), principal="token-a",
    )
    assert rejected["error"] == {
        "code": -32004, "message": "Task capacity is exhausted",
    }

    release.set()
    assert (await first)["result"]["state"] == "completed"

    async def complete(task: A2ATask) -> A2ATask:
        task.state = TaskState.COMPLETED
        return task

    protocol._process = complete
    admitted = await protocol.handle(
        _send("new-task", "second"), principal="token-a",
    )
    assert admitted["result"]["state"] == "completed"


def test_gateway_tokens_produce_distinct_opaque_principals(tmp_path) -> None:
    auth = GatewayAuth(
        GatewayAuthConfig(api_tokens=["secret-a", "secret-b"]), tmp_path,
        bound_host="127.0.0.1",
    )
    first = auth.principal_for_token("secret-a")
    second = auth.principal_for_token("secret-b")

    assert first and second and first != second
    assert "secret-a" not in first and "secret-b" not in second
    assert auth.principal_for_token("invalid") is None


@pytest.mark.asyncio
async def test_two_authenticated_peers_are_isolated_through_http_server() -> None:
    processor = MagicMock()
    processor.process_direct = AsyncMock(return_value="done")

    def authenticate(request):
        principals = {
            "Bearer secret-a": "principal-a",
            "Bearer secret-b": "principal-b",
        }
        principal = principals.get(request.headers.get("Authorization", ""))
        return principal or web.json_response({"error": "unauthorized"}, status=401)

    server = A2AServer(processor, AgentCard(), auth_fn=authenticate)

    async def rpc(body: dict, token: str) -> tuple[int, dict]:
        request = MagicMock()
        request.headers = {"Authorization": f"Bearer {token}"}
        request.json = AsyncMock(return_value=body)
        response = await server._handle_rpc(request)
        return response.status, json.loads(response.body)

    status, first = await rpc(_send("shared-id", "from-a"), "secret-a")
    assert status == 200 and first["result"]["state"] == "completed"

    _status, foreign = await rpc(
        _request("tasks/get", "shared-id", "foreign"), "secret-b",
    )
    assert foreign["error"] == {"code": -32001, "message": "Task not found"}

    _status, second = await rpc(_send("shared-id", "from-b"), "secret-b")
    assert second["result"]["id"] == "shared-id"
    session_keys = [call.kwargs["session_key"] for call in processor.process_direct.await_args_list]
    assert len(set(session_keys)) == 2
    assert all("secret-" not in key and "principal-" not in key for key in session_keys)


@pytest.mark.asyncio
async def test_a2a_processing_error_does_not_expose_internal_detail() -> None:
    processor = MagicMock()
    processor.process_direct = AsyncMock(
        side_effect=RuntimeError("provider rejected api_key=super-secret")
    )
    server = A2AServer(processor, AgentCard())
    task = A2ATask(messages=[A2AMessage.text("user", "work")])

    result = await server._process_task(task)

    assert result.state == TaskState.FAILED
    assert result.messages[-1].text_content == "Error: task processing failed"
    assert "super-secret" not in result.messages[-1].text_content
