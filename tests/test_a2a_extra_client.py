"""Extra tests for A2AClient — aiohttp ClientSession fully mocked."""

from __future__ import annotations

from unittest.mock import AsyncMock, MagicMock, patch

import pytest

from echo_agent.a2a.client import A2AClient
from echo_agent.a2a.models import AgentCard, A2ATask, TaskState


def _session(resp):
    resp.__aenter__ = AsyncMock(return_value=resp)
    resp.__aexit__ = AsyncMock(return_value=False)
    session = MagicMock()
    session.get = MagicMock(return_value=resp)
    session.post = MagicMock(return_value=resp)
    session.__aenter__ = AsyncMock(return_value=session)
    session.__aexit__ = AsyncMock(return_value=False)
    return session


class TestA2AClientDiscover:
    @pytest.mark.asyncio
    async def test_discover_parses_card(self):
        client = A2AClient()
        resp = MagicMock()
        resp.json = AsyncMock(return_value={
            "name": "RemoteAgent",
            "description": "does things",
            "version": "1.2.3",
        })
        session = _session(resp)
        with patch("aiohttp.ClientSession", return_value=session):
            card = await client.discover("https://agent.example.com/")
        assert isinstance(card, AgentCard)
        assert card.name == "RemoteAgent"
        assert card.version == "1.2.3"
        assert card.url == "https://agent.example.com/"
        # Verify well-known path is requested.
        called_url = session.get.call_args.args[0]
        assert called_url == "https://agent.example.com/.well-known/agent.json"

    @pytest.mark.asyncio
    async def test_discover_defaults_for_missing_fields(self):
        client = A2AClient()
        resp = MagicMock()
        resp.json = AsyncMock(return_value={})
        session = _session(resp)
        with patch("aiohttp.ClientSession", return_value=session):
            card = await client.discover("https://x")
        assert card.name == ""
        assert card.description == ""


class TestA2AClientSendTask:
    @pytest.mark.asyncio
    async def test_send_task_builds_jsonrpc_payload(self):
        client = A2AClient()
        resp = MagicMock()
        resp.json = AsyncMock(return_value={
            "result": {"id": "task-1", "state": "completed", "messages": []},
        })
        session = _session(resp)
        with patch("aiohttp.ClientSession", return_value=session):
            task = await client.send_task("https://x/", "hello", task_id="t-1")
        assert isinstance(task, A2ATask)
        assert task.id == "task-1"
        assert task.state == TaskState.COMPLETED

        # Inspect the posted payload.
        post_kwargs = session.post.call_args
        url = post_kwargs.args[0]
        payload = post_kwargs.kwargs["json"]
        assert url == "https://x/a2a"
        assert payload["method"] == "tasks/send"
        assert payload["params"]["id"] == "t-1"
        assert payload["params"]["message"]["role"] == "user"
        assert payload["params"]["message"]["parts"][0]["text"] == "hello"

    @pytest.mark.asyncio
    async def test_send_task_empty_result(self):
        client = A2AClient()
        resp = MagicMock()
        resp.json = AsyncMock(return_value={})
        session = _session(resp)
        with patch("aiohttp.ClientSession", return_value=session):
            task = await client.send_task("https://x", "hi")
        # from_dict on {} yields a default submitted task.
        assert task.state == TaskState.SUBMITTED


class TestA2AClientGetCancel:
    @pytest.mark.asyncio
    async def test_get_task(self):
        client = A2AClient()
        resp = MagicMock()
        resp.json = AsyncMock(return_value={
            "result": {"id": "g1", "state": "working", "messages": []},
        })
        session = _session(resp)
        with patch("aiohttp.ClientSession", return_value=session):
            task = await client.get_task("https://x", "g1")
        assert task.id == "g1"
        assert task.state == TaskState.WORKING
        payload = session.post.call_args.kwargs["json"]
        assert payload["method"] == "tasks/get"
        assert payload["params"]["id"] == "g1"

    @pytest.mark.asyncio
    async def test_cancel_task(self):
        client = A2AClient()
        resp = MagicMock()
        resp.json = AsyncMock(return_value={
            "result": {"id": "c1", "state": "canceled", "messages": []},
        })
        session = _session(resp)
        with patch("aiohttp.ClientSession", return_value=session):
            task = await client.cancel_task("https://x", "c1")
        assert task.state == TaskState.CANCELED
        payload = session.post.call_args.kwargs["json"]
        assert payload["method"] == "tasks/cancel"
