"""A2AProtocol backs tasks with a bounded TTL store."""

from __future__ import annotations

import pytest

from echo_agent.a2a.models import A2ATask, A2AMessage, TaskState
from echo_agent.a2a.protocol import A2AProtocol
from echo_agent.a2a.task_store import TaskStore


async def _echo(task: A2ATask) -> A2ATask:
    task.state = TaskState.COMPLETED
    task.messages.append(A2AMessage.text("agent", "done"))
    return task


class _Clock:
    def __init__(self) -> None:
        self.now = 0.0

    def __call__(self) -> float:
        return self.now


@pytest.mark.asyncio
async def test_protocol_uses_task_store():
    proto = A2AProtocol(_echo)
    assert isinstance(proto._tasks, TaskStore)


@pytest.mark.asyncio
async def test_completed_task_expires_from_get():
    clock = _Clock()
    proto = A2AProtocol(_echo, task_ttl_seconds=10.0, clock=clock)
    send = await proto.handle({
        "jsonrpc": "2.0", "id": 1, "method": "tasks/send",
        "params": {"id": "c1", "message": {"role": "user", "parts": [{"type": "text", "text": "hi"}]}},
    })
    assert send["result"]["state"] == "completed"
    # Within TTL: tasks/get still returns it.
    got = await proto.handle({"jsonrpc": "2.0", "id": 2, "method": "tasks/get", "params": {"id": "c1"}})
    assert got["result"]["id"] == "c1"
    # Past TTL: the terminal task is reclaimed → tasks/get errors "not found".
    clock.now = 11.0
    gone = await proto.handle({"jsonrpc": "2.0", "id": 3, "method": "tasks/get", "params": {"id": "c1"}})
    assert "error" in gone
    assert "not found" in gone["error"]["message"].lower()
