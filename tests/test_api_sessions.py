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
    # 端点调的是异步的 list_sessions_async(同步 list_sessions 在事件循环里
    # 只能看到内存缓存,且 await 一个 list 会 TypeError)。mock 必须打在真实
    # 被调方法上,避免像旧版那样用 AsyncMock 伪装同步方法而掩盖类型不匹配。
    # 字段须与真实 storage(storage/sqlite.py、session/manager.py)一致:返回
    # updated_at 而非 last_active。前端 Sessions.tsx 依赖 updated_at,旧 mock 用
    # last_active 与实现脱节,曾掩盖前端字段名 bug。
    mock_server.session_manager.list_sessions_async = AsyncMock(return_value=[
        {"key": "tg_user1", "message_count": 10, "updated_at": "2026-07-07T10:00:00"},
    ])

    app = web.Application()
    app.router.add_get("/api/v1/sessions", api.list_sessions)
    async with TestClient(TestServer(app)) as client:
        resp = await client.get("/api/v1/sessions")
        assert resp.status == 200
        data = await resp.json()
        assert len(data["sessions"]) == 1
        assert data["sessions"][0]["updated_at"] == "2026-07-07T10:00:00"


@pytest.mark.asyncio
async def test_get_session_history(mock_server, api):
    session = MagicMock()
    session.get_history.return_value = [
        {"role": "user", "content": "hello"},
        {"role": "assistant", "content": "hi"},
    ]
    # 端点用只读的 get 而非 get_or_create——一个 GET 不该把不存在的会话建出来。
    mock_server.session_manager.get = AsyncMock(return_value=session)

    app = web.Application()
    app.router.add_get("/api/v1/sessions/{key}/history", api.get_history)
    async with TestClient(TestServer(app)) as client:
        resp = await client.get("/api/v1/sessions/tg_user1/history")
        assert resp.status == 200
        data = await resp.json()
        assert len(data["messages"]) == 2
    mock_server.session_manager.get.assert_awaited_once_with("tg_user1")


@pytest.mark.asyncio
async def test_get_session_history_missing_returns_404(mock_server, api):
    """不存在的会话返回 404 而不是凭空建一个空会话:GET 必须无持久化副作用。"""
    mock_server.session_manager.get = AsyncMock(return_value=None)
    mock_server.session_manager.get_or_create = AsyncMock(
        side_effect=AssertionError("history 不得调用 get_or_create")
    )

    app = web.Application()
    app.router.add_get("/api/v1/sessions/{key}/history", api.get_history)
    async with TestClient(TestServer(app)) as client:
        resp = await client.get("/api/v1/sessions/nobody/history")
        assert resp.status == 404
