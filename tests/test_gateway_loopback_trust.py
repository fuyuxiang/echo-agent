"""Loopback-trust 补完回归。

覆盖三道闸门姿态一致后的行为：
- is_authorized(trusted=True) 对 loopback 来源放行（守住零配置 cli attach 必挂的 bug）；
- 非 loopback 仍走 allowlist，不被削弱；
- trusted 只能源自真实 socket peer，转发头伪造不生效；
- 网关端口预检占用时给出友好提示而非裸 traceback。
"""
from __future__ import annotations

import socket
from unittest.mock import MagicMock

import aiohttp
import pytest
import pytest_asyncio
from aiohttp.test_utils import make_mocked_request

from echo_agent.config.schema import GatewayAuthConfig
from echo_agent.gateway.auth import GatewayAuth
from echo_agent.gateway.server import GatewayServer


def _auth(tmp_path) -> GatewayAuth:
    # 默认 allowlist + 空白名单：这正是零配置网关锁死 cli:local 的场景。
    return GatewayAuth(GatewayAuthConfig(mode="allowlist", allowed_users=[]), tmp_path)


def test_is_authorized_has_no_trusted_bypass(tmp_path) -> None:
    auth = _auth(tmp_path)
    # 空白名单：任何身份都不因 loopback 而放行——trusted 短路已移除。
    assert not auth.is_authorized("cli", "local")
    assert not auth.is_authorized("wechat", "victim")


def test_is_authorized_allowlist_still_grants_listed_user(tmp_path) -> None:
    auth = GatewayAuth(
        GatewayAuthConfig(mode="allowlist", allowed_users=["cli:local"]), tmp_path
    )
    assert auth.is_authorized("cli", "local")
    assert not auth.is_authorized("cli", "other")


def test_is_authorized_rejects_trusted_kwarg(tmp_path) -> None:
    # P0 回归护栏：trusted 短路已彻底移除，连关键字都不再接受。
    # 若有人把 `if trusted: return True` 加回，此测试立即转红。
    import inspect
    auth = _auth(tmp_path)
    assert "trusted" not in inspect.signature(auth.is_authorized).parameters
    with pytest.raises(TypeError):
        auth.is_authorized("cli", "local", trusted=True)


def _request_with_peer(peer, headers=None):
    transport = MagicMock()
    transport.get_extra_info = lambda key, default=None: peer if key == "peername" else default
    return make_mocked_request("GET", "/ws", headers=headers or {}, transport=transport)


def test_is_loopback_peer_true_for_loopback_socket() -> None:
    req = _request_with_peer(("127.0.0.1", 51234))
    assert GatewayServer._is_loopback_peer(req) is True


def test_is_loopback_peer_false_for_remote_socket() -> None:
    req = _request_with_peer(("203.0.113.7", 51234))
    assert GatewayServer._is_loopback_peer(req) is False


def test_forwarded_header_cannot_forge_loopback_trust() -> None:
    # 真实 peer 是远程，但 X-Forwarded-For 伪造成 127.0.0.1：必须仍判为不可信。
    req = _request_with_peer(
        ("203.0.113.7", 51234),
        headers={"X-Forwarded-For": "127.0.0.1"},
    )
    assert GatewayServer._is_loopback_peer(req) is False


def test_is_loopback_peer_false_when_no_peername() -> None:
    transport = MagicMock()
    transport.get_extra_info = lambda key, default=None: default
    req = make_mocked_request("GET", "/ws", transport=transport)
    assert GatewayServer._is_loopback_peer(req) is False


def test_port_preflight_reports_when_occupied() -> None:
    from echo_agent.app import _gateway_port_in_use

    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as held:
        held.bind(("127.0.0.1", 0))
        held.listen(1)
        port = held.getsockname()[1]
        msg = _gateway_port_in_use("127.0.0.1", port)
        assert msg is not None
        assert "echo-agent cli" in msg


def test_port_preflight_clear_when_free() -> None:
    from echo_agent.app import _gateway_port_in_use

    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as probe:
        probe.bind(("127.0.0.1", 0))
        port = probe.getsockname()[1]
    # socket 已关闭，端口空出。
    assert _gateway_port_in_use("127.0.0.1", port) is None


def test_port_preflight_skips_ephemeral_port() -> None:
    from echo_agent.app import _gateway_port_in_use

    assert _gateway_port_in_use("0.0.0.0", 0) is None


# ── 端到端：默认 allowlist 网关下 loopback cli 必须握手成功 ──────────────────
#
# 这正是 bug 现象（零配置 echo-agent cli 报 认证失败：unauthorized）的真实链路：
# gateway_ws_url fixture 用 mode="open" 会掩盖它，故单独起一个 allowlist 空白名单网关。


@pytest_asyncio.fixture
async def allowlist_gateway_ws_url():
    from pathlib import Path
    from unittest.mock import AsyncMock

    from echo_agent.bus.queue import MessageBus
    from echo_agent.config.schema import (
        GatewayConfig,
        GatewayAuthConfig,
        GatewaySessionPolicyConfig,
    )
    from echo_agent.gateway.server import GatewayServer

    config = GatewayConfig(
        enabled=True,
        host="127.0.0.1",
        port=0,
        # 模式默认 allowlist、白名单为空：未补完前这会拒掉 cli:local。
        auth=GatewayAuthConfig(mode="allowlist", allowed_users=[], api_tokens=[]),
        session_policy=GatewaySessionPolicyConfig(mode="none"),
    )
    bus = MessageBus()
    session_manager = MagicMock()
    session_manager.get_or_create = AsyncMock(return_value=MagicMock(status="active"))
    server = GatewayServer(
        config=config,
        bus=bus,
        channel_manager=MagicMock(),
        session_manager=session_manager,
        workspace=Path("/tmp/echo-agent-test-loopback"),
        agent_loop=None,
    )
    await bus.start()
    await server.start()
    try:
        yield f"ws://127.0.0.1:{server.actual_port}/ws"
    finally:
        await server.stop()
        await bus.stop()


@pytest.mark.asyncio
async def test_loopback_cli_auth_ok_under_empty_allowlist(allowlist_gateway_ws_url):
    async with aiohttp.ClientSession() as s:
        async with s.ws_connect(allowlist_gateway_ws_url) as ws:
            await ws.send_json({
                "type": "auth", "platform": "cli",
                "user_id": "local", "session_key": "cli:local",
            })
            msg = await ws.receive_json()
            assert msg["type"] == "auth_ok"
            assert msg["session_key"] == "cli:local"


@pytest.mark.asyncio
async def test_cross_site_origin_rejected_before_upgrade(allowlist_gateway_ws_url):
    import aiohttp
    url = allowlist_gateway_ws_url.replace("ws://", "http://")
    async with aiohttp.ClientSession() as s:
        # 带跨站 Origin 的 WS 升级请求：应在 prepare 前 403，不升级。
        async with s.get(
            url,
            headers={
                "Origin": "https://evil.example",
                "Upgrade": "websocket",
                "Connection": "Upgrade",
                "Sec-WebSocket-Key": "dGhlIHNhbXBsZSBub25jZQ==",
                "Sec-WebSocket-Version": "13",
            },
        ) as resp:
            assert resp.status == 403


@pytest.mark.asyncio
async def test_loopback_only_client_without_cli_key_rejected(allowlist_gateway_ws_url):
    import aiohttp
    async with aiohttp.ClientSession() as s:
        async with s.ws_connect(allowlist_gateway_ws_url) as ws:
            # 空白名单 → 仅 loopback 豁免 → 无 cli key、自报 wechat:victim 必须被拒。
            await ws.send_json({
                "type": "auth", "platform": "wechat", "user_id": "victim",
            })
            msg = await ws.receive_json()
            assert msg["type"] == "error"
            assert msg["error"] == "forbidden session_key"


@pytest.mark.asyncio
async def test_loopback_cli_with_key_still_ok(allowlist_gateway_ws_url):
    import aiohttp
    async with aiohttp.ClientSession() as s:
        async with s.ws_connect(allowlist_gateway_ws_url) as ws:
            await ws.send_json({
                "type": "auth", "platform": "cli",
                "user_id": "local", "session_key": "cli:local",
            })
            msg = await ws.receive_json()
            assert msg["type"] == "auth_ok"
            assert msg["session_key"] == "cli:local"


def test_cross_site_browser_origin_with_none_sfs_is_rejected(tmp_path) -> None:
    auth = _auth(tmp_path)
    assert auth.is_cross_site_browser("https://evil.example", "none") is True


def test_cross_site_browser_detected_even_with_empty_allowlist(tmp_path) -> None:
    auth = _auth(tmp_path)  # allowed_origins 默认空
    # 明确跨站浏览器请求：默认开，判为 True（应被拒）。
    assert auth.is_cross_site_browser("https://evil.example", "cross-site") is True
    assert auth.is_cross_site_browser("https://evil.example", "") is True


def test_cross_site_browser_false_for_native_client(tmp_path) -> None:
    auth = _auth(tmp_path)
    # 原生客户端（cli/curl）：无 Origin、无 Sec-Fetch-Site → 非浏览器 → False。
    assert auth.is_cross_site_browser("", "") is False


def test_cross_site_browser_false_for_same_origin(tmp_path) -> None:
    auth = _auth(tmp_path)
    assert auth.is_cross_site_browser("http://127.0.0.1:9000", "same-origin") is False
    assert auth.is_cross_site_browser("", "none") is False


def test_cross_site_browser_allowlisted_origin_passes(tmp_path) -> None:
    from echo_agent.config.schema import GatewayAuthConfig
    auth = GatewayAuth(
        GatewayAuthConfig(mode="open", allowed_origins=["https://app.example"]), tmp_path
    )
    # 显式放行的 Origin：不算跨站浏览器攻击 → False。
    assert auth.is_cross_site_browser("https://app.example", "cross-site") is False
    # 其它跨站 Origin 仍判为 True。
    assert auth.is_cross_site_browser("https://evil.example", "cross-site") is True


def _msg_request(body, *, headers=None, peer=("127.0.0.1", 5555)):
    from unittest.mock import MagicMock
    from aiohttp.test_utils import make_mocked_request
    transport = MagicMock()
    transport.get_extra_info = lambda key, default=None: peer if key == "peername" else default
    req = make_mocked_request("POST", "/api/v1/message", headers=headers or {}, transport=transport)
    async def _json():
        return body
    req.json = _json  # type: ignore[method-assign]
    return req


@pytest.mark.asyncio
async def test_message_rejects_cross_site_browser(tmp_path) -> None:
    from test_gateway_server import _make_gateway
    gw, _ = _make_gateway()
    resp = await gw._handle_message(_msg_request(
        {"platform": "api", "user_id": "u1", "text": "hi"},
        headers={"Origin": "https://evil.example", "Sec-Fetch-Site": "cross-site"},
    ))
    assert resp.status == 403


@pytest.mark.asyncio
async def test_port_preflight_runs_before_bootstrap(monkeypatch, tmp_path) -> None:
    import socket as _socket
    from echo_agent import app as app_mod

    bootstrap_called = False

    async def _spy_bootstrap(*a, **k):
        nonlocal bootstrap_called
        bootstrap_called = True
        raise AssertionError("bootstrap must not run when port is occupied")

    monkeypatch.setattr(app_mod, "bootstrap", _spy_bootstrap)

    # 占住一个端口，让预检命中。
    with _socket.socket(_socket.AF_INET, _socket.SOCK_STREAM) as held:
        held.bind(("127.0.0.1", 0))
        held.listen(1)
        port = held.getsockname()[1]
        # host/port 直接由入参提供，跳过 bootstrap 也能预检。
        await app_mod.run_gateway(host="127.0.0.1", port=port, workspace=str(tmp_path))

    assert bootstrap_called is False

