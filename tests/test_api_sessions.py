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
async def test_history_hides_the_compressors_injected_summary_pair(mock_server, api):
    """压缩注入的摘要对不得出现在人类可读的历史里。

    压缩器把摘要写成 role=user、把确认写成 role=assistant(见
    agent/compression/assembler.py),这样模型才会当参考材料读。但 messages 在压缩
    重写后会落盘,于是这两条进入了存储的"历史":前端按 role 分左右,摘要因此被渲染
    成蓝色用户气泡——用户看到自己"说过"一段机器生成的摘要。
    """
    from echo_agent.agent.compression.assembler import SUMMARY_ACK, SUMMARY_PREFIX

    session = Session(key="cli:local")
    session.add_message("user", "帮我算个数")
    session.add_message("assistant", "好的")
    session.add_message("user", SUMMARY_PREFIX + "此前用户要求计算。")
    session.add_message("assistant", SUMMARY_ACK)
    session.add_message("user", "继续")

    mock_server.session_manager.get = AsyncMock(return_value=session)

    app = web.Application()
    app.router.add_get("/api/v1/sessions/{key}/history", api.get_history)
    async with TestClient(TestServer(app)) as client:
        resp = await client.get("/api/v1/sessions/cli%3Alocal/history")
        data = await resp.json()

    contents = [m["content"] for m in data["messages"]]
    assert contents == ["帮我算个数", "好的", "继续"]
    assert not any(SUMMARY_PREFIX in c for c in contents)
    assert SUMMARY_ACK not in contents
    # 存储本身不动:过滤只作用于展示视图。
    assert len(session.messages) == 5


@pytest.mark.asyncio
async def test_history_tags_tool_traffic_instead_of_passing_it_off_as_chat(mock_server, api):
    """工具调用与工具结果要带 internal 标记,由前端折叠显示。

    它们真实存在、排查问题时正是要看的内容,所以不能丢;但它们不是对话轮次,
    前端旧逻辑「非 user 即 Agent 气泡」会把工具输出显示成 Agent 说的话。
    """
    session = Session(key="cli:local")
    session.add_message("user", "北京天气")
    session.add_message("assistant", "", tool_calls=[
        {"id": "c1", "function": {"name": "web_search", "arguments": "{}"}},
    ])
    session.add_message("tool", "晴 28C", tool_call_id="c1", name="web_search")
    session.add_message("assistant", "北京今天晴。")

    mock_server.session_manager.get = AsyncMock(return_value=session)

    app = web.Application()
    app.router.add_get("/api/v1/sessions/{key}/history", api.get_history)
    async with TestClient(TestServer(app)) as client:
        resp = await client.get("/api/v1/sessions/cli%3Alocal/history")
        data = await resp.json()

    flags = [(m["role"], m.get("internal", False)) for m in data["messages"]]
    assert flags == [
        ("user", False),
        ("assistant", True),   # 只发起了工具调用,没有面向用户的内容
        ("tool", True),
        ("assistant", False),  # 真正的回复
    ]
    # internal 标记不得回流到 LLM 读的那份列表。
    assert all("internal" not in m for m in session.messages)


@pytest.mark.asyncio
async def test_history_limit_is_range_checked(mock_server, api):
    """limit 越界返回 400,而不是被 Python 切片解释成别的意思。

    此前只做 int() 转换:limit=0 走 messages[-0:] 返回**全部**历史,limit=-1 返回
    除首条外的全部,超大值同样返回全部——一个"限制"参数实际取消了限制,并把整段
    会话的序列化成本交给任意调用方触发。
    """
    session = Session(key="cli:local")
    for i in range(10):
        session.add_message("user", f"m{i}")
    mock_server.session_manager.get = AsyncMock(return_value=session)

    app = web.Application()
    app.router.add_get("/api/v1/sessions/{key}/history", api.get_history)
    async with TestClient(TestServer(app)) as client:
        for bad in ("0", "-1", "1000000"):
            resp = await client.get(f"/api/v1/sessions/cli%3Alocal/history?limit={bad}")
            assert resp.status == 400, f"limit={bad} 应被拒绝"

        resp = await client.get("/api/v1/sessions/cli%3Alocal/history?limit=3")
        assert resp.status == 200
        data = await resp.json()
        # 返回最近 3 条,total 仍是完整记录数——客户端据此判断是否还有更早的历史。
        assert [m["content"] for m in data["messages"]] == ["m7", "m8", "m9"]
        assert data["returned"] == 3
        assert data["total"] == 10


@pytest.mark.asyncio
async def test_history_total_counts_the_whole_transcript(mock_server, api):
    """total 是整段可见历史的长度,不是本页条数。

    旧实现是 len(messages)(即本页长度),在默认 limit 下恒等于返回条数,客户端
    完全无法据此判断还有没有更早的历史。
    """
    session = Session(key="cli:local")
    for i in range(4):
        session.add_message("user", f"m{i}")
    mock_server.session_manager.get = AsyncMock(return_value=session)

    app = web.Application()
    app.router.add_get("/api/v1/sessions/{key}/history", api.get_history)
    async with TestClient(TestServer(app)) as client:
        resp = await client.get("/api/v1/sessions/cli%3Alocal/history?limit=2")
        data = await resp.json()

    assert data["total"] == 4
    assert data["returned"] == 2


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


@pytest.mark.asyncio
async def test_turn_status_endpoints_use_durable_store(mock_server, api):
    store = MagicMock()
    store.get = AsyncMock(return_value={"event_id": "e1", "status": "incomplete"})
    store.list_session = AsyncMock(return_value=[
        {"event_id": "e1", "status": "incomplete"},
    ])
    mock_server._agent_loop = MagicMock(turn_runs=store)

    app = web.Application()
    app.router.add_get("/api/v1/turns/{event_id}", api.get_turn)
    app.router.add_get("/api/v1/sessions/{key}/turns", api.list_turns)
    async with TestClient(TestServer(app)) as client:
        resp = await client.get("/api/v1/turns/e1")
        assert resp.status == 200
        assert (await resp.json())["turn"]["status"] == "incomplete"

        resp = await client.get("/api/v1/sessions/cli%3Alocal/turns?limit=1")
        assert resp.status == 200
        assert (await resp.json())["turns"][0]["event_id"] == "e1"
    store.get.assert_awaited_once_with("e1")
    store.list_session.assert_awaited_once_with("cli:local", limit=1)


@pytest.mark.asyncio
async def test_turn_status_limit_is_bounded(mock_server, api):
    mock_server._agent_loop = MagicMock(turn_runs=MagicMock())
    app = web.Application()
    app.router.add_get("/api/v1/sessions/{key}/turns", api.list_turns)
    async with TestClient(TestServer(app)) as client:
        for value in ("0", "101", "oops"):
            resp = await client.get(f"/api/v1/sessions/s/turns?limit={value}")
            assert resp.status == 400
