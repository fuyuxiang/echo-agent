"""Gateway interrupt-frame E2E: a {"type":"interrupt"} ws frame is turned into
an internal is_control /__interrupt__ inbound event (bypassing the rate limiter),
and rejected before auth. Mirrors test_gateway_ws_session_key.py's harness."""

import asyncio
from pathlib import Path
from unittest.mock import AsyncMock, MagicMock

import aiohttp
import pytest
import pytest_asyncio

from echo_agent.bus.queue import MessageBus
from echo_agent.config.schema import (
    GatewayConfig, GatewayAuthConfig, GatewaySessionPolicyConfig,
)
from echo_agent.gateway.server import GatewayServer


@pytest_asyncio.fixture
async def gateway_capturing_inbound():
    """Start a gateway that records every inbound event the bus dispatches, so a
    test can assert what the ws layer synthesized."""
    captured: list = []

    config = GatewayConfig(
        enabled=True, host="127.0.0.1", port=0,
        auth=GatewayAuthConfig(mode="open", api_tokens=[]),
        session_policy=GatewaySessionPolicyConfig(mode="none"),
    )
    bus = MessageBus()
    session_manager = MagicMock()
    session_manager.get_or_create = AsyncMock(return_value=MagicMock(status="active"))

    async def capture(event):
        captured.append(event)

    bus.subscribe_inbound(capture)

    server = GatewayServer(
        config=config, bus=bus, channel_manager=MagicMock(),
        session_manager=session_manager,
        workspace=Path("/tmp/echo-agent-test-interrupt"), agent_loop=None,
    )
    await bus.start()
    await server.start()
    try:
        yield f"ws://127.0.0.1:{server.actual_port}/ws", captured
    finally:
        await server.stop()
        await bus.stop()


@pytest.mark.asyncio
async def test_interrupt_frame_synthesizes_control_inbound(gateway_capturing_inbound):
    url, captured = gateway_capturing_inbound
    async with aiohttp.ClientSession() as s:
        async with s.ws_connect(url) as ws:
            await ws.send_json({
                "type": "auth", "platform": "cli",
                "user_id": "alice", "session_key": "cli:alice",
            })
            assert (await ws.receive_json())["type"] == "auth_ok"

            await ws.send_json({"type": "interrupt"})
            assert (await ws.receive_json())["type"] == "accepted"

    # The ws layer must have produced exactly one control interrupt event.
    await asyncio.sleep(0.05)
    interrupts = [e for e in captured if e.text.strip() == "/__interrupt__"]
    assert len(interrupts) == 1
    ev = interrupts[0]
    assert ev.is_control is True                 # 绕限流
    assert ev.session_key == "cli:alice"         # 按会话寻址


@pytest.mark.asyncio
async def test_interrupt_before_auth_is_rejected(gateway_capturing_inbound):
    url, captured = gateway_capturing_inbound
    async with aiohttp.ClientSession() as s:
        async with s.ws_connect(url) as ws:
            await ws.send_json({"type": "interrupt"})
            msg = await ws.receive_json()
            assert msg["type"] == "error"
            assert "authenticate" in msg["error"]

    await asyncio.sleep(0.05)
    assert not any(e.text.strip() == "/__interrupt__" for e in captured)
