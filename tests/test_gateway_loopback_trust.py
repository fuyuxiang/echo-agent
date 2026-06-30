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


def test_loopback_trust_bypasses_empty_allowlist(tmp_path) -> None:
    auth = _auth(tmp_path)
    # 未信任 → 空白名单拒绝（原 bug 现象）。
    assert not auth.is_authorized("cli", "local")
    # loopback 可信 → 放行，三道闸门姿态一致。
    assert auth.is_authorized("cli", "local", trusted=True)


def test_non_loopback_still_enforces_allowlist(tmp_path) -> None:
    auth = _auth(tmp_path)
    # 远程来源（trusted=False）绝不因 loopback 豁免而放行。
    assert not auth.is_authorized("cli", "local", trusted=False)
    assert not auth.is_authorized("slack", "u2", trusted=False)


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

