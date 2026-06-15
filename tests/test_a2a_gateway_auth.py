"""Integration tests: A2A routes respect Gateway auth and channel identity."""

from __future__ import annotations

from unittest.mock import AsyncMock, MagicMock

import pytest

from aiohttp import web

from echo_agent.a2a.models import AgentCard
from echo_agent.a2a.server import A2AServer


def _make_agent_card():
    return AgentCard(
        name="test-agent",
        description="A test agent",
        url="http://localhost:8080",
        version="1.0.0",
    )


def _make_processor(response_text="Hello from agent"):
    processor = AsyncMock()
    processor.process_direct = AsyncMock(return_value=response_text)
    return processor


def _make_request(body, *, headers=None):
    request = MagicMock()

    async def _json():
        return body

    request.json = _json
    request.headers = headers or {}
    return request


def _jsonrpc_send_task(task_id="t1", text="hello"):
    return {
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tasks/send",
        "params": {
            "id": task_id,
            "message": {"role": "user", "parts": [{"type": "text", "text": text}]},
        },
    }


class TestA2AAuthEnforcement:
    """A2A RPC endpoint must respect gateway token auth."""

    @pytest.mark.asyncio
    async def test_a2a_rejects_without_token_when_auth_configured(self):
        processor = _make_processor()
        card = _make_agent_card()

        def auth_fn(request):
            token = request.headers.get("Authorization", "")
            if token != "Bearer secret":
                return web.json_response({"error": "unauthorized"}, status=401)
            return None

        server = A2AServer(processor, card, auth_fn=auth_fn)
        request = _make_request(_jsonrpc_send_task(), headers={})
        response = await server._handle_rpc(request)
        assert response.status == 401

    @pytest.mark.asyncio
    async def test_a2a_allows_with_valid_token(self):
        processor = _make_processor()
        card = _make_agent_card()

        def auth_fn(request):
            token = request.headers.get("Authorization", "")
            if token != "Bearer secret":
                return web.json_response({"error": "unauthorized"}, status=401)
            return None

        server = A2AServer(processor, card, auth_fn=auth_fn)
        request = _make_request(
            _jsonrpc_send_task(),
            headers={"Authorization": "Bearer secret"},
        )
        response = await server._handle_rpc(request)
        assert response.status == 200

    @pytest.mark.asyncio
    async def test_a2a_no_auth_fn_allows_all(self):
        processor = _make_processor()
        card = _make_agent_card()
        server = A2AServer(processor, card, auth_fn=None)
        request = _make_request(_jsonrpc_send_task())
        response = await server._handle_rpc(request)
        assert response.status == 200


class TestA2AChannelIdentity:
    """A2A requests must use channel='a2a', not 'cli'."""

    @pytest.mark.asyncio
    async def test_a2a_calls_process_direct_with_a2a_channel(self):
        processor = _make_processor("response")
        card = _make_agent_card()
        server = A2AServer(processor, card)

        request = _make_request(_jsonrpc_send_task(task_id="task-123", text="do something"))
        await server._handle_rpc(request)

        processor.process_direct.assert_called_once()
        call_kwargs = processor.process_direct.call_args.kwargs
        assert call_kwargs.get("channel") == "a2a"

    @pytest.mark.asyncio
    async def test_a2a_session_key_prefixed(self):
        processor = _make_processor("response")
        card = _make_agent_card()
        server = A2AServer(processor, card)

        request = _make_request(_jsonrpc_send_task(task_id="task-456", text="test"))
        await server._handle_rpc(request)

        call_kwargs = processor.process_direct.call_args.kwargs
        assert call_kwargs.get("session_key", "").startswith("a2a:")
