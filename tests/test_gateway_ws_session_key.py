import asyncio

import aiohttp
import pytest
import pytest_asyncio

from echo_agent.gateway.ws_session import resolve_client_session_key


def test_empty_request_falls_back_to_gateway_key():
    key, err = resolve_client_session_key(None, platform="wechat", chat_id="u1")
    assert err == ""
    assert key == "gateway:wechat:u1"


def test_cli_prefix_accepted_verbatim():
    key, err = resolve_client_session_key("cli:alice", platform="cli", chat_id="alice")
    assert err == ""
    assert key == "cli:alice"


def test_non_whitelisted_prefix_rejected():
    key, err = resolve_client_session_key(
        "gateway:wechat:victim", platform="cli", chat_id="alice"
    )
    assert key is None
    assert "prefix" in err


def test_blank_string_after_strip_falls_back():
    key, err = resolve_client_session_key("   ", platform="cli", chat_id="bob")
    assert err == ""
    assert key == "gateway:cli:bob"


@pytest.mark.asyncio
async def test_ws_auth_accepts_cli_session_key(gateway_ws_url):
    async with aiohttp.ClientSession() as s:
        async with s.ws_connect(gateway_ws_url) as ws:
            await ws.send_json({
                "type": "auth", "platform": "cli",
                "user_id": "alice", "session_key": "cli:alice",
            })
            msg = await ws.receive_json()
            assert msg["type"] == "auth_ok"
            assert msg["session_key"] == "cli:alice"


@pytest.mark.asyncio
async def test_ws_auth_rejects_impersonation(gateway_ws_url):
    async with aiohttp.ClientSession() as s:
        async with s.ws_connect(gateway_ws_url) as ws:
            await ws.send_json({
                "type": "auth", "platform": "cli",
                "user_id": "alice", "session_key": "gateway:wechat:victim",
            })
            msg = await ws.receive_json()
            assert msg["type"] == "error"


# ── 闭环出站投递回归（守住 cli: 自带 session_key 丢消息 bug）────────────────────
#
# fixture gateway_ws_url 用 agent_loop=None，跑不到出站投递，是原 bug 的盲区。
# 这里启动真实 bus，订阅 inbound 模拟 agent：收到入站后构造一条出站事件
# （metadata 拷贝入站、含 _session_key），走真实 bus.publish_outbound →
# _handle_outbound → broadcast_to_ws 链路，断言对应 ws 客户端能收到回复。


@pytest_asyncio.fixture
async def gateway_with_echo_agent():
    """Start a gateway whose bus echoes each inbound back as an outbound reply."""
    from pathlib import Path
    from unittest.mock import AsyncMock, MagicMock

    from echo_agent.bus.queue import MessageBus
    from echo_agent.bus.events import OutboundEvent, ContentBlock, ContentType
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
        auth=GatewayAuthConfig(mode="open", api_tokens=[]),
        session_policy=GatewaySessionPolicyConfig(mode="none"),
    )

    bus = MessageBus()
    session_manager = MagicMock()
    session_manager.get_or_create = AsyncMock(return_value=MagicMock(status="active"))

    async def fake_agent(event):
        # 模拟 agent：构造出站回复，metadata 拷贝入站（含 _session_key），走 bus 出站。
        out = OutboundEvent(
            channel=event.channel,
            chat_id=event.chat_id,
            content=[ContentBlock(type=ContentType.TEXT, text=f"echo:{event.text}")],
            metadata=dict(event.metadata),
            is_final=True,
        )
        await bus.publish_outbound(out)

    bus.subscribe_inbound(fake_agent)

    server = GatewayServer(
        config=config,
        bus=bus,
        channel_manager=MagicMock(),
        session_manager=session_manager,
        workspace=Path("/tmp/echo-agent-test-ws"),
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
async def test_cli_session_key_receives_outbound_reply(gateway_with_echo_agent):
    """自带 session_key=cli:alice 的客户端必须收到出站回复（守住原 bug）。"""
    async with aiohttp.ClientSession() as s:
        async with s.ws_connect(gateway_with_echo_agent) as ws:
            await ws.send_json({
                "type": "auth", "platform": "cli",
                "user_id": "alice", "session_key": "cli:alice",
            })
            assert (await ws.receive_json())["type"] == "auth_ok"

            await ws.send_json({"type": "message", "text": "ping"})
            # 先收 accepted，再收 agent 出站回复
            msgs = []
            while len(msgs) < 2:
                m = await asyncio.wait_for(ws.receive_json(), timeout=5)
                msgs.append(m)
                if m["type"] == "message":
                    break
            reply = next(m for m in msgs if m["type"] == "message")
            assert reply["text"] == "echo:ping"


@pytest_asyncio.fixture
async def gateway_with_fresh_metadata_agent():
    """Gateway whose agent builds the outbound with a BRAND-NEW metadata dict.

    Mimics the heartbeat/skills/send_file outbound paths that do NOT copy the
    inbound ``event.metadata`` (so no ``_session_key`` is carried through). This
    is the case the 治标 fix (出站读 metadata['_session_key']) silently dropped
    for cli: clients. Under the root fix (投递键=出站重算键) it must still hit.
    """
    from pathlib import Path
    from unittest.mock import AsyncMock, MagicMock

    from echo_agent.bus.queue import MessageBus
    from echo_agent.bus.events import OutboundEvent, ContentBlock, ContentType
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
        auth=GatewayAuthConfig(mode="open", api_tokens=[]),
        session_policy=GatewaySessionPolicyConfig(mode="none"),
    )

    bus = MessageBus()
    session_manager = MagicMock()
    session_manager.get_or_create = AsyncMock(return_value=MagicMock(status="active"))

    async def fake_agent(event):
        # 关键：用全新 metadata（不含 _session_key），模拟 heartbeat 那样
        # out.metadata = {"_inbound_event_id": ...}，而非 dict(event.metadata)。
        out = OutboundEvent(
            channel=event.channel,
            chat_id=event.chat_id,
            content=[ContentBlock(type=ContentType.TEXT, text=f"echo:{event.text}")],
            is_final=True,
        )
        out.metadata = {"_inbound_event_id": event.event_id}
        await bus.publish_outbound(out)

    bus.subscribe_inbound(fake_agent)

    server = GatewayServer(
        config=config,
        bus=bus,
        channel_manager=MagicMock(),
        session_manager=session_manager,
        workspace=Path("/tmp/echo-agent-test-ws-fresh"),
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
async def test_cli_client_receives_reply_when_outbound_drops_metadata(
    gateway_with_fresh_metadata_agent,
):
    """根治核心优势：出站不拷贝入站 metadata（无 _session_key）时，cli: 客户端仍能收到。

    若回退到治标（_ws_clients 注册键=身份键 cli:alice，出站靠 metadata 透传），
    这条必然失败——因为出站重算键 gateway:cli:alice 与注册键 cli:alice 不一致。
    """
    async with aiohttp.ClientSession() as s:
        async with s.ws_connect(gateway_with_fresh_metadata_agent) as ws:
            await ws.send_json({
                "type": "auth", "platform": "cli",
                "user_id": "alice", "session_key": "cli:alice",
            })
            assert (await ws.receive_json())["type"] == "auth_ok"

            await ws.send_json({"type": "message", "text": "ping"})
            msgs = []
            while len(msgs) < 2:
                m = await asyncio.wait_for(ws.receive_json(), timeout=5)
                msgs.append(m)
                if m["type"] == "message":
                    break
            reply = next(m for m in msgs if m["type"] == "message")
            assert reply["text"] == "echo:ping"


@pytest.mark.asyncio
async def test_legacy_client_still_receives_outbound_reply(gateway_with_echo_agent):
    """老客户端（不带 session_key）出站仍可达，证明向后兼容未破坏。"""
    async with aiohttp.ClientSession() as s:
        async with s.ws_connect(gateway_with_echo_agent) as ws:
            await ws.send_json({
                "type": "auth", "platform": "wechat", "user_id": "u1",
            })
            auth_ok = await ws.receive_json()
            assert auth_ok["type"] == "auth_ok"
            assert auth_ok["session_key"] == "gateway:wechat:u1"

            await ws.send_json({"type": "message", "text": "hi"})
            msgs = []
            while len(msgs) < 2:
                m = await asyncio.wait_for(ws.receive_json(), timeout=5)
                msgs.append(m)
                if m["type"] == "message":
                    break
            reply = next(m for m in msgs if m["type"] == "message")
            assert reply["text"] == "echo:hi"

