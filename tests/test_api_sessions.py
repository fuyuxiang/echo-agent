# tests/test_api_sessions.py
import pytest
from unittest.mock import MagicMock, AsyncMock
from aiohttp import web
from aiohttp.test_utils import TestServer, TestClient

from echo_agent.gateway.api.sessions import SessionsAPI
from echo_agent.session.manager import Session


@pytest.fixture
def mock_server():
    server = MagicMock()
    server._require_admin_token = MagicMock(return_value=None)
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
    # 用真实 Session 而非 MagicMock:历史端点必须走 get_display_history(展示全量),
    # 不能是 get_history(LLM 用的、从 last_consolidated 起切的紧凑视图)。MagicMock
    # 对任意属性都返回可用桩,会把"端点调错方法"这类契约漂移一起掩盖掉。
    session = Session(key="tg_user1")
    session.add_message("user", "hello")
    session.add_message("assistant", "hi")
    # 端点用只读的 get 而非 get_or_create——一个 GET 不该把不存在的会话建出来。
    mock_server.session_manager.get = AsyncMock(return_value=session)

    app = web.Application()
    app.router.add_get("/api/v1/sessions/{key}/history", api.get_history)
    async with TestClient(TestServer(app)) as client:
        resp = await client.get("/api/v1/sessions/tg_user1/history")
        assert resp.status == 200
        data = await resp.json()
        assert len(data["messages"]) == 2
        assert [m["role"] for m in data["messages"]] == ["user", "assistant"]
    mock_server.session_manager.get.assert_awaited_once_with("tg_user1")


@pytest.mark.asyncio
async def test_history_shows_fully_consolidated_session(mock_server, api):
    """回归:一个已完全 consolidated 的会话,历史端点仍要返回全部消息。

    这是原始 bug:端点曾调 get_history,它从 messages[last_consolidated:] 起切,
    当 last_consolidated == 消息数(cli:local / weixin 的真实状态)时返回空,
    dashboard 因此显示空白。get_display_history 从全量 messages 切,不受影响。
    """
    session = Session(key="cli:local")
    for i in range(5):
        session.add_message("user", f"q{i}")
        session.add_message("assistant", f"a{i}")
    # 模拟 consolidation 已推进到尾部:get_history 会返回空,展示端点不该受此影响。
    session.last_consolidated = len(session.messages)
    assert session.get_history() == []  # 钉住 LLM 视图确实为空,凸显两条路径的差异

    mock_server.session_manager.get = AsyncMock(return_value=session)

    app = web.Application()
    app.router.add_get("/api/v1/sessions/{key}/history", api.get_history)
    async with TestClient(TestServer(app)) as client:
        resp = await client.get("/api/v1/sessions/cli%3Alocal/history")
        assert resp.status == 200
        data = await resp.json()
        assert len(data["messages"]) == 10, "已 consolidated 的会话历史不该为空"


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


@pytest.mark.asyncio
async def test_non_admin_request_rejected():
    """非管理员请求应被 _require_admin_token 拒绝,返回 403。"""
    server = MagicMock()
    server._require_admin_token = MagicMock(
        return_value=web.json_response({"error": "admin authorization required"}, status=403)
    )
    server.session_manager = MagicMock()

    api = SessionsAPI(server)

    app = web.Application()
    app.router.add_get("/api/v1/sessions", api.list_sessions)
    async with TestClient(TestServer(app)) as client:
        resp = await client.get("/api/v1/sessions")
        assert resp.status == 403
        data = await resp.json()
        assert data["error"] == "admin authorization required"
