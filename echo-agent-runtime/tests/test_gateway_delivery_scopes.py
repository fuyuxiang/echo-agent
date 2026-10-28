"""Interactive delivery must never cross explicit conversation boundaries."""

import asyncio
from dataclasses import replace
from unittest.mock import AsyncMock, MagicMock

import aiohttp
import pytest
import pytest_asyncio

from echo_agent.bus.events import InboundEvent, OutboundEvent
from echo_agent.bus.queue import MessageBus
from echo_agent.config.schema import GatewayAuthConfig, GatewayConfig, GatewaySessionPolicyConfig
from echo_agent.gateway.server import GatewayServer
from echo_agent.gateway.ws_delivery import GatewayDeliveryScopes, connection_key


def incoming(key="cli:A", event_id="turn-A", channel="gateway:ws", chat_id="demo-chat"):
    return InboundEvent.text_message(
        channel=channel, sender_id="demo-user", chat_id=chat_id, text="hello",
        event_id=event_id, session_key_override=key,
    )


def outgoing(event, **fields):
    return OutboundEvent.text_reply(
        channel=event.channel, chat_id=event.chat_id, text=event.session_key,
        metadata={"_inbound_event_id": event.event_id}, **fields,
    )


def test_explicit_scopes_keep_same_chat_separate():
    scopes = GatewayDeliveryScopes()
    a, b = incoming(), incoming("cli:B", "turn-B")
    for event in (a, b):
        scopes.remember(event)
    clients = {connection_key("gateway:ws:demo-chat", event.session_key): object() for event in (a, b)}
    assert scopes.resolve(outgoing(a), clients) == ("gateway:ws:demo-chat", "cli:A")
    assert scopes.resolve(outgoing(b), clients) == ("gateway:ws:demo-chat", "cli:B")


def test_expired_correlation_does_not_select_another_conversation():
    scopes = GatewayDeliveryScopes(max_turns=1)
    a, b = incoming(), incoming("cli:B", "turn-B")
    scopes.remember(a)
    scopes.remember(b)
    clients = {connection_key("gateway:ws:demo-chat", b.session_key): object()}
    assert scopes.resolve(outgoing(a), clients) is None
    # Durable recovery keeps the typed origin even after the bounded cache evicts it.
    assert scopes.resolve(outgoing(a, delivery_origin=replace(a)), clients) == ("gateway:ws:demo-chat", "cli:A")
    scopes.clear()
    assert scopes.resolve(outgoing(b), clients) is None


def test_unattributed_notifications_require_an_unambiguous_target():
    scopes = GatewayDeliveryScopes()
    notification = OutboundEvent.text_reply(channel="gateway:ws", chat_id="demo-chat", text="hello")
    clients = {("gateway:ws:demo-chat", "cli:A"): object()}
    assert scopes.resolve(notification, clients) == ("gateway:ws:demo-chat", "cli:A")
    clients[("gateway:ws:demo-chat", "cli:B")] = object()
    assert scopes.resolve(notification, clients) is None


def test_native_tool_delivery_can_address_a_different_interactive_target():
    scopes = GatewayDeliveryScopes()
    native = incoming(channel="telegram", chat_id="native")
    scopes.remember(native)
    notification = outgoing(native)
    notification.channel, notification.chat_id = "gateway:ws", "demo-chat"
    clients = {("gateway:ws:demo-chat", "cli:B"): object()}
    assert scopes.resolve(notification, clients) == ("gateway:ws:demo-chat", "cli:B")


@pytest_asyncio.fixture
async def echo_gateway(tmp_path):
    bus = MessageBus()
    sessions = MagicMock()
    sessions.get_or_create = AsyncMock(return_value=MagicMock(status="active"))
    gateway = GatewayServer(
        GatewayConfig(enabled=True, host="127.0.0.1", port=0,
                      auth=GatewayAuthConfig(mode="open"),
                      session_policy=GatewaySessionPolicyConfig(mode="none")),
        bus, MagicMock(), sessions, tmp_path,
    )
    captured, receipts = [], []
    release = asyncio.Event()

    async def echo(event):
        if event.is_control:
            return
        captured.append(event)
        if event.text == "delayed":
            await release.wait()
        receipts.append(await bus.publish_outbound(outgoing(event)))

    bus.subscribe_inbound(echo)
    await bus.start()
    await gateway.start()
    try:
        yield gateway, captured, receipts, release
    finally:
        release.set()
        await gateway.stop()
        await bus.stop()


async def authenticate(client, url, key):
    ws = await client.ws_connect(url)
    await ws.send_json({"type": "auth", "platform": "web", "user_id": "demo-user",
                        "chat_id": "demo-chat", "session_key": key})
    assert (await ws.receive_json(timeout=2))["session_key"] == key
    return ws


async def reply(ws):
    while True:
        frame = await ws.receive_json(timeout=2)
        if frame["type"] == "message":
            return frame


async def wait_for_count(items, count):
    async with asyncio.timeout(2):
        while len(items) < count:
            await asyncio.sleep(0)


@pytest.mark.asyncio
async def test_two_live_sessions_with_identical_chat_ids_receive_only_their_own_replies(echo_gateway):
    gateway, _, receipts, _ = echo_gateway
    async with aiohttp.ClientSession() as client:
        url = f"http://127.0.0.1:{gateway.actual_port}/ws"
        a = await authenticate(client, url, "cli:A")
        b = await authenticate(client, url, "cli:B")
        await asyncio.gather(a.send_json({"type": "message", "text": "a"}),
                             b.send_json({"type": "message", "text": "b"}))
        assert (await reply(a))["text"] == "cli:A"
        assert (await reply(b))["text"] == "cli:B"
        await wait_for_count(receipts, 2)
        assert all(receipt.ok for receipt in receipts)
        await a.close()
        await b.send_json({"type": "message", "text": "still connected"})
        assert (await reply(b))["text"] == "cli:B"
        await b.close()


@pytest.mark.asyncio
async def test_http_reply_for_a_does_not_broadcast_into_b(echo_gateway):
    gateway, _, _, _ = echo_gateway
    base = f"http://127.0.0.1:{gateway.actual_port}"
    async with aiohttp.ClientSession() as client:
        b = await authenticate(client, base + "/ws", "cli:B")
        async with client.post(base + "/api/v1/message", json={
            "platform": "web", "user_id": "demo-user", "chat_id": "demo-chat",
            "session_key": "cli:A", "text": "a", "wait": True,
        }) as response:
            assert response.status == 200
            assert (await response.json())["reply"]["text"] == "cli:A"
        await b.send_json({"type": "message", "text": "b"})
        assert (await reply(b))["text"] == "cli:B"
        await b.close()


@pytest.mark.asyncio
async def test_late_reply_after_switch_does_not_enter_new_session(echo_gateway):
    gateway, captured, receipts, release = echo_gateway
    async with aiohttp.ClientSession() as client:
        url = f"http://127.0.0.1:{gateway.actual_port}/ws"
        a = await authenticate(client, url, "cli:A")
        await a.send_json({"type": "message", "text": "delayed"})
        await wait_for_count(captured, 1)
        await a.close()
        b = await authenticate(client, url, "cli:B")
        release.set()
        await b.send_json({"type": "message", "text": "b"})
        assert (await reply(b))["text"] == "cli:B"
        await wait_for_count(receipts, 2)
        assert any(not receipt.ok for receipt in receipts)
        await b.close()


@pytest.mark.asyncio
async def test_old_connection_close_does_not_cancel_the_reconnected_session(echo_gateway):
    gateway, _, _, _ = echo_gateway
    controls = []

    async def capture(event):
        if event.is_control:
            controls.append(event)

    gateway._bus.subscribe_inbound(capture)
    async with aiohttp.ClientSession() as client:
        url = f"http://127.0.0.1:{gateway.actual_port}/ws"
        old = await authenticate(client, url, "cli:A")
        current = await authenticate(client, url, "cli:A")
        await old.close()
        await current.send_json({"type": "message", "text": "after reconnect"})
        assert (await reply(current))["text"] == "cli:A"
        assert controls == []
        await current.close()
