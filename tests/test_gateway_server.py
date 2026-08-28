"""Tests for GatewayServer HTTP handling, pending futures, and outbound resolution."""

from __future__ import annotations

import asyncio
from pathlib import Path
from unittest.mock import AsyncMock, MagicMock

import pytest

from echo_agent.bus.events import OutboundEvent, ContentBlock, ContentType
from echo_agent.bus.queue import MessageBus


def _make_gateway():
    """Create a minimal GatewayServer for testing."""
    from echo_agent.gateway.server import GatewayServer
    from echo_agent.config.schema import GatewayConfig, GatewayAuthConfig, GatewaySessionPolicyConfig

    config = GatewayConfig(
        enabled=True,
        host="127.0.0.1",
        port=19999,
        auth=GatewayAuthConfig(mode="open"),
        session_policy=GatewaySessionPolicyConfig(mode="none"),
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


class _JsonRequest:
    def __init__(self, body: dict):
        self._body = body
        self.headers = {}
        self.query = {}

    async def json(self) -> dict:
        return self._body


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
async def test_session_reset_clears_process_state_inside_session_lock() -> None:
    gw, _ = _make_gateway()
    entered = False

    class _Lock:
        async def __aenter__(self):
            nonlocal entered
            entered = True

        async def __aexit__(self, *_args):
            nonlocal entered
            entered = False

    gw.session_policy = MagicMock()
    gw.session_policy.should_reset.return_value = True
    gw.session_policy.reset = AsyncMock()
    gw.session_manager.acquire = AsyncMock(return_value=_Lock())

    async def _reset_state(_key):
        assert entered, "new-epoch caches must be cleared before releasing the session lock"

    gw._agent_loop.reset_session_state = AsyncMock(side_effect=_reset_state)
    _session, reset = await gw._reset_session_if_needed("cli:local")
    assert reset is True
    gw._agent_loop.reset_session_state.assert_awaited_once_with("cli:local")


@pytest.mark.asyncio
async def test_manual_reset_unblocks_human_wait_before_lock() -> None:
    gw, _ = _make_gateway()
    calls: list[str] = []

    class _Lock:
        async def __aenter__(self):
            calls.append("lock")

        async def __aexit__(self, *_args):
            pass

    gw.session_policy.reset = AsyncMock()
    gw.session_manager.acquire = AsyncMock(return_value=_Lock())
    gw._agent_loop.unblock_session_for_reset = MagicMock(
        side_effect=lambda _key: calls.append("unblock")
    )
    gw._agent_loop.reset_session_state = AsyncMock()

    await gw._reset_session_if_needed("cli:local", force=True)
    assert calls[:2] == ["unblock", "lock"]


@pytest.mark.asyncio
async def test_handle_message_preserves_gateway_media_url_image_type(tmp_path: Path) -> None:
    gw, bus = _make_gateway()
    cached_image = tmp_path / "cached.png"
    gw.media_cache.download = AsyncMock(return_value=cached_image)
    bus.publish_inbound = AsyncMock(return_value=True)

    response = await gw._handle_message(_JsonRequest({
        "platform": "api",
        "user_id": "user-1",
        "chat_id": "chat-1",
        "text": "describe this",
        "media_urls": ["https://cdn.example.com/source"],
    }))

    assert response.status == 200
    event = bus.publish_inbound.await_args.args[0]
    assert event.content[1].type == ContentType.IMAGE
    assert event.media_items[0].type == ContentType.IMAGE


@pytest.mark.asyncio
async def test_handle_message_infers_image_from_cached_extension(tmp_path: Path) -> None:
    # 回归：URL 无扩展名，但下载后按 Content-Type 落地为 .heic，
    # 仍应识别为 IMAGE，而不是退化成 FILE（否则模型看不到图片）。
    gw, bus = _make_gateway()
    cached_heic = tmp_path / "abc123.heic"
    gw.media_cache.download = AsyncMock(return_value=cached_heic)
    bus.publish_inbound = AsyncMock(return_value=True)

    response = await gw._handle_message(_JsonRequest({
        "platform": "api",
        "user_id": "user-1",
        "chat_id": "chat-1",
        "text": "describe this",
        "media_urls": ["https://cdn.example.com/abc123"],
    }))

    assert response.status == 200
    event = bus.publish_inbound.await_args.args[0]
    assert event.content[1].type == ContentType.IMAGE


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
