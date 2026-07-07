# tests/test_api_sessions.py
import pytest
from unittest.mock import MagicMock, AsyncMock
from aiohttp import web
from aiohttp.test_utils import TestServer, TestClient

from echo_agent.gateway.api.sessions import SessionsAPI


@pytest.fixture
def mock_server():
    server = MagicMock()
    server._require_api_token = MagicMock(return_value=None)
    server.session_manager = MagicMock()
    return server


@pytest.fixture
def api(mock_server):
    return SessionsAPI(mock_server)


@pytest.mark.asyncio
async def test_list_sessions(mock_server, api):
    mock_server.session_manager.list_sessions = AsyncMock(return_value=[
        {"key": "tg_user1", "message_count": 10, "last_active": "2026-07-07T10:00:00"},
    ])

    app = web.Application()
    app.router.add_get("/api/v1/sessions", api.list_sessions)
    async with TestClient(TestServer(app)) as client:
        resp = await client.get("/api/v1/sessions")
        assert resp.status == 200
        data = await resp.json()
        assert len(data["sessions"]) == 1


@pytest.mark.asyncio
async def test_get_session_history(mock_server, api):
    session = MagicMock()
    session.get_history.return_value = [
        {"role": "user", "content": "hello"},
        {"role": "assistant", "content": "hi"},
    ]
    mock_server.session_manager.get_or_create = AsyncMock(return_value=session)

    app = web.Application()
    app.router.add_get("/api/v1/sessions/{key}/history", api.get_history)
    async with TestClient(TestServer(app)) as client:
        resp = await client.get("/api/v1/sessions/tg_user1/history")
        assert resp.status == 200
        data = await resp.json()
        assert len(data["messages"]) == 2
