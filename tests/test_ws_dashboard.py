# tests/test_ws_dashboard.py
import asyncio
from unittest.mock import MagicMock, AsyncMock

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
    server_mock.auth.validate_token = MagicMock(return_value=True)

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
    info["server_mock"].auth.validate_token = MagicMock(return_value=False)

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
