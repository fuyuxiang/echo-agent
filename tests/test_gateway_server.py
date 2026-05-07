"""Tests for GatewayServer HTTP handling, pending futures, and outbound resolution."""

from __future__ import annotations

import asyncio
from unittest.mock import AsyncMock, MagicMock, patch

import pytest

from echo_agent.bus.events import OutboundEvent, ContentBlock, ContentType
from echo_agent.bus.queue import MessageBus


def _make_gateway():
    """Create a minimal GatewayServer for testing."""
    from echo_agent.gateway.server import GatewayServer
    from echo_agent.config.schema import GatewayConfig, GatewayAuthConfig

    config = GatewayConfig(
        enabled=True,
        host="127.0.0.1",
        port=19999,
        auth=GatewayAuthConfig(mode="open"),
    )
    bus = MessageBus()
    channel_manager = MagicMock()
    session_manager = MagicMock()
    session_manager.get_or_create = AsyncMock(return_value=MagicMock(status="active"))
    workspace = MagicMock()
    agent_loop = MagicMock()

    gw = GatewayServer(
        config=config,
        bus=bus,
        channel_manager=channel_manager,
        session_manager=session_manager,
        workspace=workspace,
        agent_loop=agent_loop,
    )
    return gw, bus


@pytest.mark.asyncio
async def test_handle_outbound_resolves_future() -> None:
    gw, _ = _make_gateway()

    future = asyncio.get_event_loop().create_future()
    gw._pending_http["event-123"] = future

    event = OutboundEvent(
        channel="gateway:api",
        chat_id="chat-1",
        content=[ContentBlock(type=ContentType.TEXT, text="response")],
    )
    event.is_final = True
    event.metadata = {"_inbound_event_id": "event-123"}

    await gw._handle_outbound(event)

    assert future.done()
    result = future.result()
    assert result["text"] == "response" or "content" in result or "text" in str(result)


@pytest.mark.asyncio
async def test_handle_outbound_drop_skipped() -> None:
    gw, _ = _make_gateway()

    future = asyncio.get_event_loop().create_future()
    gw._pending_http["event-456"] = future

    event = OutboundEvent(
        channel="gateway:api",
        chat_id="chat-1",
        content=[ContentBlock(type=ContentType.TEXT, text="dropped")],
    )
    event.is_final = True
    event.metadata = {"_drop": True, "_inbound_event_id": "event-456"}

    await gw._handle_outbound(event)

    assert not future.done()  # future not resolved because _drop


@pytest.mark.asyncio
async def test_handle_outbound_invalid_state_protected() -> None:
    gw, _ = _make_gateway()

    future = asyncio.get_event_loop().create_future()
    future.cancel()  # pre-cancel
    gw._pending_http["event-789"] = future

    event = OutboundEvent(
        channel="gateway:api",
        chat_id="chat-1",
        content=[ContentBlock(type=ContentType.TEXT, text="late")],
    )
    event.is_final = True
    event.metadata = {"_inbound_event_id": "event-789"}

    # Should not raise
    await gw._handle_outbound(event)


@pytest.mark.asyncio
async def test_pending_http_capacity_limit() -> None:
    gw, _ = _make_gateway()

    # Fill up pending_http to capacity
    for i in range(gw._MAX_PENDING_HTTP):
        gw._pending_http[f"event-{i}"] = asyncio.get_event_loop().create_future()

    assert len(gw._pending_http) == gw._MAX_PENDING_HTTP


@pytest.mark.asyncio
async def test_handle_outbound_non_gateway_channel_ignored() -> None:
    gw, _ = _make_gateway()

    future = asyncio.get_event_loop().create_future()
    gw._pending_http["event-abc"] = future

    event = OutboundEvent(
        channel="weixin",
        chat_id="chat-1",
        content=[ContentBlock(type=ContentType.TEXT, text="hello")],
    )
    event.is_final = True
    event.metadata = {"_inbound_event_id": "event-abc"}

    await gw._handle_outbound(event)

    assert not future.done()  # not resolved for non-gateway channel
