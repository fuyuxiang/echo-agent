# tests/test_ws_dashboard.py
import asyncio
from unittest.mock import MagicMock

import aiohttp
import pytest
import pytest_asyncio

from aiohttp import web


@pytest_asyncio.fixture
async def dashboard_ws_url():
    """Start a minimal aiohttp app with DashboardWebSocket on loopback."""
    from echo_agent.gateway.ws_dashboard import DashboardWebSocket

    server_mock = MagicMock()
    server_mock.auth = MagicMock()
    # Default: token validation returns True
    server_mock.auth.authenticate_token = MagicMock(return_value=True)
    # Cross-site gate + heartbeat config the hardened handler now reads.
    server_mock.auth.is_cross_site_browser = MagicMock(return_value=False)
    server_mock.auth.audit = MagicMock()
    server_mock._config = MagicMock()
    server_mock._config.ws_heartbeat_seconds = 0

    dws = DashboardWebSocket(server_mock)

    app = web.Application()
    app.router.add_get("/ws/dashboard", dws.handle)

    runner = web.AppRunner(app)
    await runner.setup()
    site = web.TCPSite(runner, "127.0.0.1", 0)
    await site.start()
    port = runner.addresses[0][1]

    try:
        yield {
            "url": f"ws://127.0.0.1:{port}/ws/dashboard",
            "dws": dws,
            "server_mock": server_mock,
        }
    finally:
        await site.stop()
        await runner.cleanup()


@pytest.mark.asyncio
async def test_ws_dashboard_auth_required(dashboard_ws_url):
    """Auth with invalid token should return auth_error."""
    info = dashboard_ws_url
    info["server_mock"].auth.authenticate_token = MagicMock(return_value=False)

    async with aiohttp.ClientSession() as s:
        async with s.ws_connect(info["url"]) as ws:
            await ws.send_json({"type": "auth", "token": "bad_token"})
            msg = await asyncio.wait_for(ws.receive_json(), timeout=3)
            assert msg["type"] == "auth_error"


@pytest.mark.asyncio
async def test_ws_dashboard_subscribe_and_receive(dashboard_ws_url):
    """Auth OK -> subscribe -> broadcast -> client receives event."""
    info = dashboard_ws_url
    dws = info["dws"]

    async with aiohttp.ClientSession() as s:
        async with s.ws_connect(info["url"]) as ws:
            await ws.send_json({"type": "auth", "token": "valid"})
            msg = await asyncio.wait_for(ws.receive_json(), timeout=3)
            assert msg["type"] == "auth_ok"

            await ws.send_json({"type": "subscribe", "channels": ["tasks"]})
            msg = await asyncio.wait_for(ws.receive_json(), timeout=3)
            assert msg["type"] == "subscribed"

            await dws.broadcast("task_created", {"id": "t_abc", "title": "test"})
            msg = await asyncio.wait_for(ws.receive_json(), timeout=3)
            assert msg["type"] == "task_created"
            assert msg["payload"]["id"] == "t_abc"


@pytest.mark.asyncio
async def test_cross_site_origin_rejected_before_upgrade(dashboard_ws_url):
    """A cross-site page must be refused at the HTTP layer. Once the socket is
    upgraded the browser's onopen has already fired, so rejecting inside the
    message loop is too late — hence asserting 403, not a close frame."""
    info = dashboard_ws_url
    info["server_mock"].auth.is_cross_site_browser = MagicMock(return_value=True)

    async with aiohttp.ClientSession() as s:
        with pytest.raises(aiohttp.WSServerHandshakeError) as excinfo:
            await s.ws_connect(info["url"], headers={"Origin": "https://evil.example.com"})
        assert excinfo.value.status == 403


@pytest.mark.asyncio
async def test_cross_site_rejection_is_audited(dashboard_ws_url):
    info = dashboard_ws_url
    info["server_mock"].auth.is_cross_site_browser = MagicMock(return_value=True)

    async with aiohttp.ClientSession() as s:
        with pytest.raises(aiohttp.WSServerHandshakeError):
            await s.ws_connect(info["url"], headers={"Origin": "https://evil.example.com"})

    actions = [c.args[0] for c in info["server_mock"].auth.audit.call_args_list]
    assert "dashboard_ws_auth" in actions


@pytest.mark.asyncio
async def test_unauthenticated_socket_closed_after_timeout(dashboard_ws_url, monkeypatch):
    """An idle unauthenticated socket must not sit there indefinitely."""
    from echo_agent.gateway import ws_common

    monkeypatch.setattr(ws_common, "DASHBOARD_AUTH_TIMEOUT_SECONDS", 0.2)

    async with aiohttp.ClientSession() as s:
        async with s.ws_connect(dashboard_ws_url["url"]) as ws:
            msg = await asyncio.wait_for(ws.receive(), timeout=3)
            assert msg.type in (aiohttp.WSMsgType.CLOSE, aiohttp.WSMsgType.CLOSED)


@pytest.mark.asyncio
async def test_authenticated_socket_survives_auth_timeout(dashboard_ws_url, monkeypatch):
    """The timeout bounds only the pre-auth window. An authenticated subscriber
    is a legitimate long-lived connection and must stay open while idle."""
    from echo_agent.gateway import ws_common

    monkeypatch.setattr(ws_common, "DASHBOARD_AUTH_TIMEOUT_SECONDS", 0.2)
    info = dashboard_ws_url

    async with aiohttp.ClientSession() as s:
        async with s.ws_connect(info["url"]) as ws:
            await ws.send_json({"type": "auth", "token": "good"})
            assert (await asyncio.wait_for(ws.receive_json(), timeout=3))["type"] == "auth_ok"
            await ws.send_json({"type": "subscribe", "channels": ["tasks"]})
            await asyncio.wait_for(ws.receive_json(), timeout=3)

            # Idle well past the pre-auth timeout, then prove the socket is live.
            await asyncio.sleep(0.6)
            await info["dws"].broadcast("task_created", {"id": "t_idle"})
            msg = await asyncio.wait_for(ws.receive_json(), timeout=3)
            assert msg["type"] == "task_created"


@pytest.mark.asyncio
async def test_successful_auth_is_audited(dashboard_ws_url):
    info = dashboard_ws_url
    async with aiohttp.ClientSession() as s:
        async with s.ws_connect(info["url"]) as ws:
            await ws.send_json({"type": "auth", "token": "good"})
            msg = await asyncio.wait_for(ws.receive_json(), timeout=3)
            assert msg["type"] == "auth_ok"

    ok_calls = [
        c for c in info["server_mock"].auth.audit.call_args_list
        if c.args and c.args[0] == "dashboard_ws_auth" and c.kwargs.get("ok") is True
    ]
    assert ok_calls


@pytest.mark.asyncio
async def test_unknown_channel_returns_structured_error(dashboard_ws_url):
    """Subscribing to a channel nothing emits into used to succeed silently, so
    the UI could not tell a dead subscription from a quiet one."""
    info = dashboard_ws_url
    async with aiohttp.ClientSession() as s:
        async with s.ws_connect(info["url"]) as ws:
            await ws.send_json({"type": "auth", "token": "good"})
            await asyncio.wait_for(ws.receive_json(), timeout=3)
            await ws.send_json({"type": "subscribe", "channels": ["tasks", "not_a_channel"]})
            msg = await asyncio.wait_for(ws.receive_json(), timeout=3)

    assert msg["type"] == "subscribe_error"
    assert msg["unknown"] == ["not_a_channel"]
    assert msg["channels"] == ["tasks"]


@pytest.mark.asyncio
async def test_unauthenticated_connection_cap(dashboard_ws_url, monkeypatch):
    """Cap concurrent unauthenticated sockets so they cannot be piled up."""
    from echo_agent.gateway import ws_common

    monkeypatch.setattr(ws_common, "MAX_UNAUTHENTICATED_CLIENTS", 1)
    monkeypatch.setattr(ws_common, "DASHBOARD_AUTH_TIMEOUT_SECONDS", 5.0)
    info = dashboard_ws_url

    async with aiohttp.ClientSession() as s:
        async with s.ws_connect(info["url"]):
            with pytest.raises(aiohttp.WSServerHandshakeError) as excinfo:
                await s.ws_connect(info["url"])
            assert excinfo.value.status == 503
