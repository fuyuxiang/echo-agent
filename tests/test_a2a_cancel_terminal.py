"""A2A protocol tests: tasks/cancel must respect terminal states."""

import pytest

from echo_agent.a2a.models import A2ATask, A2AMessage, TaskState
from echo_agent.a2a.protocol import A2AProtocol


async def _echo(task: A2ATask) -> A2ATask:
    task.state = TaskState.COMPLETED
    task.messages.append(A2AMessage.text("agent", "done"))
    return task


@pytest.mark.asyncio
async def test_cancel_rejects_completed_task():
    proto = A2AProtocol(_echo)
    # Run a task to completion via tasks/send.
    send = await proto.handle({
        "jsonrpc": "2.0", "id": 1, "method": "tasks/send",
        "params": {"id": "c1", "message": {"role": "user", "parts": [{"type": "text", "text": "hi"}]}},
    })
    assert send["result"]["state"] == "completed"

    # Cancelling a completed task must be an error, not a silent state flip.
    cancel = await proto.handle({
        "jsonrpc": "2.0", "id": 2, "method": "tasks/cancel", "params": {"id": "c1"},
    })
    assert "error" in cancel
    assert "cannot be canceled" in cancel["error"]["message"]


@pytest.mark.asyncio
async def test_cancel_unknown_task_errors():
    proto = A2AProtocol(_echo)
    resp = await proto.handle({
        "jsonrpc": "2.0", "id": 1, "method": "tasks/cancel", "params": {"id": "nope"},
    })
    assert "error" in resp
    assert "not found" in resp["error"]["message"].lower()


@pytest.mark.asyncio
async def test_cancel_allows_non_terminal_task():
    # A task that exists but is not terminal can be canceled.
    proto = A2AProtocol(_echo)
    task = A2ATask(id="c2", state=TaskState.WORKING)
    proto._tasks["c2"] = task
    resp = await proto.handle({
        "jsonrpc": "2.0", "id": 1, "method": "tasks/cancel", "params": {"id": "c2"},
    })
    assert resp["result"]["state"] == "canceled"
