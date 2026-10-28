# tests/test_ws_auth_timeout.py
"""The main WS must not hold an unauthenticated socket open indefinitely.

The endpoint had an Origin gate and a heartbeat but no bound on the pre-auth
window: authentication is the first frame of the message loop, so a peer that
connected and never sent an `auth` frame kept its connection slot for as long
as it cared to. These tests pin the bound *and* the other half of the contract —
an authenticated client may idle as long as it likes, because a TUI turn can
run far longer than the pre-auth timeout.

Reuses the `gateway_ws_url` fixture from conftest (a real open-mode
GatewayServer on loopback), so the whole handshake path is exercised.
"""
import asyncio

import aiohttp
import pytest


@pytest.mark.asyncio
async def test_main_ws_closes_unauthenticated_socket(gateway_ws_url, monkeypatch):
    """An open socket that never authenticates is closed by the server."""
    from echo_agent.gateway import ws_common

    monkeypatch.setattr(ws_common, "DASHBOARD_AUTH_TIMEOUT_SECONDS", 0.2)

    async with aiohttp.ClientSession() as s:
        async with s.ws_connect(gateway_ws_url) as ws:
            msg = await asyncio.wait_for(ws.receive(), timeout=3)
            assert msg.type in (aiohttp.WSMsgType.CLOSE, aiohttp.WSMsgType.CLOSED)


@pytest.mark.asyncio
async def test_pre_auth_timeout_survives_non_auth_traffic(gateway_ws_url, monkeypatch):
    """Chatting without authenticating does not buy more time.

    The gate is "has this socket authenticated", not "has it been quiet": a
    peer that keeps sending `message` frames it is not entitled to send still
    hits the bound.
    """
    from echo_agent.gateway import ws_common

    monkeypatch.setattr(ws_common, "DASHBOARD_AUTH_TIMEOUT_SECONDS", 0.3)

    async with aiohttp.ClientSession() as s:
        async with s.ws_connect(gateway_ws_url) as ws:
            await ws.send_json({"type": "message", "text": "hi"})
            first = await asyncio.wait_for(ws.receive_json(), timeout=3)
            assert first["type"] == "error"

            while True:
                msg = await asyncio.wait_for(ws.receive(), timeout=3)
                if msg.type in (aiohttp.WSMsgType.CLOSE, aiohttp.WSMsgType.CLOSED):
                    break
                assert msg.type == aiohttp.WSMsgType.TEXT


@pytest.mark.asyncio
async def test_authenticated_socket_survives_auth_timeout(gateway_ws_url, monkeypatch):
    """The timeout bounds only the pre-auth window.

    An authenticated TUI sits idle while a turn runs; killing it there would
    drop the user's session mid-answer.
    """
    from echo_agent.gateway import ws_common

    monkeypatch.setattr(ws_common, "DASHBOARD_AUTH_TIMEOUT_SECONDS", 0.2)

    async with aiohttp.ClientSession() as s:
        async with s.ws_connect(gateway_ws_url) as ws:
            await ws.send_json({
                "type": "auth", "platform": "cli",
                "user_id": "alice", "session_key": "cli:alice",
            })
            assert (await asyncio.wait_for(ws.receive_json(), timeout=3))["type"] == "auth_ok"

            # Idle well past the pre-auth timeout, then prove the socket is live.
            await asyncio.sleep(0.7)
            await ws.send_json({"type": "ping"})
            msg = await asyncio.wait_for(ws.receive_json(), timeout=3)
            assert msg["type"] == "pong"


@pytest.mark.asyncio
async def test_cross_site_origin_rejected_before_upgrade(gateway_ws_url):
    """The shared gate still refuses cross-site browser upgrades pre-prepare()."""
    async with aiohttp.ClientSession() as s:
        with pytest.raises(aiohttp.WSServerHandshakeError) as exc:
            await s.ws_connect(gateway_ws_url, headers={"Origin": "https://evil.example.com"})
        assert exc.value.status == 403


@pytest.mark.asyncio
async def test_native_client_without_origin_still_accepted(gateway_ws_url):
    """The CLI sends no Origin / Sec-Fetch-Site and must pass the gate."""
    async with aiohttp.ClientSession() as s:
        async with s.ws_connect(gateway_ws_url) as ws:
            await ws.send_json({
                "type": "auth", "platform": "cli",
                "user_id": "alice", "session_key": "cli:alice",
            })
            assert (await asyncio.wait_for(ws.receive_json(), timeout=3))["type"] == "auth_ok"
