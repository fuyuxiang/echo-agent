"""Gateway skill.* WS frames E2E: exercises the real dispatch in server.py.

The handler-level tests in echo_agent/gateway/__tests__/ws_skill_test.py mirror
the routing logic; these drive the actual socket so the wiring itself is covered:
  - the store comes from agent_loop.skill_store (same instance as the HTTP API),
  - skills.enabled=false (skill_store=None) answers with an error, not a 500,
  - enable/disable demand an admin-scoped token like their HTTP counterparts,
  - list stays readable with a plain api token.
"""

from pathlib import Path
from unittest.mock import AsyncMock, MagicMock

import aiohttp
import pytest
import pytest_asyncio

from echo_agent.bus.queue import MessageBus
from echo_agent.config.schema import (
    GatewayAuthConfig, GatewayConfig, GatewaySessionPolicyConfig,
)
from echo_agent.gateway.server import GatewayServer
from echo_agent.skills.store import SkillStore


def _make_skill(store: SkillStore, name: str, *, enabled: bool = True) -> None:
    content = f"---\nname: {name}\ndescription: desc-{name}\n---\n\n# {name}\n"
    error = store.create_skill(name, content)
    assert error is None, error
    if not enabled:
        store.persist_disable(name)


async def _serve(tmp_path: Path, *, store, api_tokens=None, admin_tokens=None):
    """Start a gateway whose agent_loop exposes ``store`` as its skill_store."""
    config = GatewayConfig(
        enabled=True, host="127.0.0.1", port=0,
        # mode 管的是用户授权,与令牌作用域无关:令牌单独配。
        auth=GatewayAuthConfig(
            mode="open",
            api_tokens=api_tokens or [],
            admin_tokens=admin_tokens or [],
        ),
        session_policy=GatewaySessionPolicyConfig(mode="none"),
    )
    bus = MessageBus()
    session_manager = MagicMock()
    session_manager.get_or_create = AsyncMock(return_value=MagicMock(status="active"))

    agent_loop = MagicMock()
    agent_loop.skill_store = store

    server = GatewayServer(
        config=config, bus=bus, channel_manager=MagicMock(),
        session_manager=session_manager,
        workspace=tmp_path, agent_loop=agent_loop,
    )
    await bus.start()
    await server.start()
    return server, bus, f"ws://127.0.0.1:{server.actual_port}/ws"


@pytest_asyncio.fixture
async def open_gateway(tmp_path):
    """No tokens configured (loopback deployment): admin check passes."""
    store = SkillStore(user_dir=tmp_path / "skills")
    server, bus, url = await _serve(tmp_path, store=store)
    try:
        yield url, store
    finally:
        await server.stop()
        await bus.stop()


@pytest_asyncio.fixture
async def scoped_gateway(tmp_path):
    """Separate api / admin tokens, so scope separation is real."""
    store = SkillStore(user_dir=tmp_path / "skills")
    server, bus, url = await _serve(
        tmp_path, store=store, api_tokens=["api-tok"], admin_tokens=["admin-tok"],
    )
    try:
        yield url, store
    finally:
        await server.stop()
        await bus.stop()


@pytest_asyncio.fixture
async def skills_off_gateway(tmp_path):
    """skills.enabled=false is represented by skill_store=None."""
    server, bus, url = await _serve(tmp_path, store=None)
    try:
        yield url
    finally:
        await server.stop()
        await bus.stop()


async def _auth(ws, token=None):
    frame = {
        "type": "auth", "platform": "cli",
        "user_id": "alice", "session_key": "cli:alice",
    }
    if token is not None:
        frame["token"] = token
    await ws.send_json(frame)
    assert (await ws.receive_json())["type"] == "auth_ok"


@pytest.mark.asyncio
async def test_skill_list_reads_agent_store(open_gateway):
    """list 必须读 agent_loop.skill_store —— 与 HTTP API 同一实例。"""
    url, store = open_gateway
    _make_skill(store, "ppt-author")
    _make_skill(store, "summarize", enabled=False)

    async with aiohttp.ClientSession() as s:
        async with s.ws_connect(url) as ws:
            await _auth(ws)
            await ws.send_json({"type": "skill.list", "request_id": "r-1"})
            frame = await ws.receive_json()

    assert frame["type"] == "skill.list_result"
    assert frame["request_id"] == "r-1"
    by_name = {sk["name"]: sk for sk in frame["skills"]}
    assert by_name["ppt-author"]["enabled"] is True
    assert by_name["summarize"]["enabled"] is False


@pytest.mark.asyncio
async def test_skill_enable_mutates_agent_store(open_gateway):
    """enable 的效果要落到 agent 那个 store 上,否则改了也不生效。"""
    url, store = open_gateway
    _make_skill(store, "ppt-author", enabled=False)

    async with aiohttp.ClientSession() as s:
        async with s.ws_connect(url) as ws:
            await _auth(ws)
            await ws.send_json({"type": "skill.enable", "name": "ppt-author"})
            assert (await ws.receive_json())["type"] == "accepted"

    assert store.is_disabled("ppt-author") is False


@pytest.mark.asyncio
async def test_skill_disable_mutates_agent_store(open_gateway):
    url, store = open_gateway
    _make_skill(store, "ppt-author")

    async with aiohttp.ClientSession() as s:
        async with s.ws_connect(url) as ws:
            await _auth(ws)
            await ws.send_json({"type": "skill.disable", "name": "ppt-author"})
            assert (await ws.receive_json())["type"] == "accepted"

    assert store.is_disabled("ppt-author") is True


@pytest.mark.asyncio
async def test_unknown_skill_returns_error(open_gateway):
    url, _store = open_gateway
    async with aiohttp.ClientSession() as s:
        async with s.ws_connect(url) as ws:
            await _auth(ws)
            await ws.send_json({
                "type": "skill.enable", "name": "ghost", "request_id": "r-2",
            })
            frame = await ws.receive_json()

    assert frame["type"] == "error"
    assert "ghost" in frame["message"]
    assert frame["request_id"] == "r-2"


@pytest.mark.asyncio
async def test_skill_frames_rejected_before_auth(open_gateway):
    """pre-auth 闸门:握手未完成不得触达 store。"""
    url, store = open_gateway
    _make_skill(store, "ppt-author")

    async with aiohttp.ClientSession() as s:
        async with s.ws_connect(url) as ws:
            await ws.send_json({"type": "skill.disable", "name": "ppt-author"})
            frame = await ws.receive_json()

    assert frame["error"] == "authenticate first"
    assert store.is_disabled("ppt-author") is False


@pytest.mark.asyncio
async def test_skills_disabled_system_reports_error(skills_off_gateway):
    """skills.enabled=false 时如实回错,而不是 AttributeError。"""
    url = skills_off_gateway
    async with aiohttp.ClientSession() as s:
        async with s.ws_connect(url) as ws:
            await _auth(ws)
            await ws.send_json({"type": "skill.list"})
            frame = await ws.receive_json()

    assert frame["type"] == "error"
    assert "disabled" in frame["message"]


@pytest.mark.asyncio
async def test_api_token_cannot_enable_skill(scoped_gateway):
    """只有 api 作用域的客户端不能翻转技能开关(与 HTTP toggle 对齐)。"""
    url, store = scoped_gateway
    _make_skill(store, "ppt-author", enabled=False)

    async with aiohttp.ClientSession() as s:
        async with s.ws_connect(url) as ws:
            await _auth(ws, token="api-tok")
            await ws.send_json({"type": "skill.enable", "name": "ppt-author"})
            frame = await ws.receive_json()

    assert frame["type"] == "error"
    assert "admin" in frame["message"]
    assert store.is_disabled("ppt-author") is True  # 状态未变


@pytest.mark.asyncio
async def test_api_token_can_still_list_skills(scoped_gateway):
    """只读的 list 不受 admin 闸门影响。"""
    url, store = scoped_gateway
    _make_skill(store, "ppt-author")

    async with aiohttp.ClientSession() as s:
        async with s.ws_connect(url) as ws:
            await _auth(ws, token="api-tok")
            await ws.send_json({"type": "skill.list"})
            frame = await ws.receive_json()

    assert frame["type"] == "skill.list_result"


@pytest.mark.asyncio
async def test_admin_token_can_enable_skill(scoped_gateway):
    """admin 令牌放行。"""
    url, store = scoped_gateway
    _make_skill(store, "ppt-author", enabled=False)

    async with aiohttp.ClientSession() as s:
        async with s.ws_connect(url) as ws:
            await _auth(ws, token="admin-tok")
            await ws.send_json({"type": "skill.enable", "name": "ppt-author"})
            assert (await ws.receive_json())["type"] == "accepted"

    assert store.is_disabled("ppt-author") is False
