# tests/test_ws_auth_deadline.py
"""The pre-auth bound is absolute, not a per-frame idle timeout.

Both WS endpoints wrapped each `receive()` in `wait_for(..., AUTH_TIMEOUT)`, which
restarts its clock on every frame. What that enforces is "N seconds of silence",
not "N seconds to authenticate": a peer that keeps sending frames it is not
entitled to send renews its own deadline forever. On the dashboard endpoint that
holds one of only MAX_UNAUTHENTICATED_CLIENTS slots, so eight chattering peers
deny the dashboard to everyone.

test_ws_auth_timeout.py has a test named for this case, but it cannot detect the
bug: it sends a single frame and then waits, so the reset it would have to catch
never gets exercised twice. These tests keep talking for longer than the budget.
"""
from __future__ import annotations

import asyncio
import time

import aiohttp
import pytest

from echo_agent.gateway import ws_common


async def _chatter_until_closed(ws, send, *, budget_multiple: float = 4.0):
    """Keep sending un-entitled frames and report whether the server closed us.

    The frames must be spread across real time, and the spacing has to stay under
    the auth budget. Both matter:

    - Spread across time, because the deadline is measured in seconds. A tight
      loop finishes in milliseconds (an unauthenticated frame gets an immediate
      error reply, so `receive()` never blocks) and would report "still open" no
      matter which timeout semantics are in force.
    - Spaced UNDER the budget, because that is what a per-frame timeout can never
      fire on. If the gap exceeded the budget an idle timeout would also close us
      and the test would pass against the very behaviour it exists to reject.
    """
    budget = ws_common.DASHBOARD_AUTH_TIMEOUT_SECONDS
    gap = budget / 4
    deadline = time.monotonic() + budget * budget_multiple
    while time.monotonic() < deadline:
        try:
            await send()
        except (ConnectionResetError, aiohttp.ClientError):
            return True
        try:
            msg = await asyncio.wait_for(ws.receive(), timeout=gap)
        except asyncio.TimeoutError:
            continue
        if msg.type in (
            aiohttp.WSMsgType.CLOSE, aiohttp.WSMsgType.CLOSED, aiohttp.WSMsgType.CLOSING,
        ):
            return True
        # An immediate error reply leaves the rest of the gap unspent; sleep it
        # off so the frames actually straddle the budget.
        await asyncio.sleep(gap)
    return False


@pytest.mark.asyncio
async def test_main_ws_deadline_is_not_renewed_by_traffic(gateway_ws_url, monkeypatch):
    """Chatter must not buy more time on the main WS.

    Sends unauthenticated frames continuously at an interval well under the
    budget — the exact pattern a per-frame timeout can never fire on."""
    monkeypatch.setattr(ws_common, "DASHBOARD_AUTH_TIMEOUT_SECONDS", 0.4)

    async with aiohttp.ClientSession() as s:
        async with s.ws_connect(gateway_ws_url) as ws:
            closed = await _chatter_until_closed(
                ws, lambda: ws.send_json({"type": "message", "text": "still here"}),
            )
            assert closed, "unauthenticated socket outlived its absolute deadline"


@pytest.mark.asyncio
async def test_dashboard_ws_deadline_is_not_renewed_by_traffic(
    gateway_ws_url, monkeypatch,
):
    """Same guarantee for the dashboard endpoint, where the slots are scarce."""
    monkeypatch.setattr(ws_common, "DASHBOARD_AUTH_TIMEOUT_SECONDS", 0.4)
    dashboard_url = gateway_ws_url + "/dashboard"

    async with aiohttp.ClientSession() as s:
        async with s.ws_connect(dashboard_url) as ws:
            # Unparseable on purpose: the loop `continue`s on bad JSON, which
            # under the old code was a free deadline reset.
            closed = await _chatter_until_closed(ws, lambda: ws.send_str("not json"))
            assert closed, "anonymous dashboard socket held its slot past the deadline"


@pytest.mark.asyncio
async def test_unauthenticated_slot_is_released_after_the_deadline(
    gateway_ws_url, monkeypatch,
):
    """The scarce resource is actually reclaimed.

    MAX_UNAUTHENTICATED_CLIENTS is 8; a peer whose deadline expires must give its
    slot back, or the ceiling turns into a permanent denial of the dashboard."""
    monkeypatch.setattr(ws_common, "DASHBOARD_AUTH_TIMEOUT_SECONDS", 0.3)
    dashboard_url = gateway_ws_url + "/dashboard"

    async with aiohttp.ClientSession() as s:
        # Fill every anonymous slot and let them all expire.
        sockets = [await s.ws_connect(dashboard_url) for _ in range(
            ws_common.MAX_UNAUTHENTICATED_CLIENTS
        )]
        for ws in sockets:
            msg = await asyncio.wait_for(ws.receive(), timeout=3)
            assert msg.type in (aiohttp.WSMsgType.CLOSE, aiohttp.WSMsgType.CLOSED)
            await ws.close()

        # A fresh connection still gets in rather than being refused with 503.
        async with s.ws_connect(dashboard_url) as ws:
            await ws.send_json({"type": "auth", "token": ""})
            reply = await asyncio.wait_for(ws.receive_json(), timeout=3)
            assert reply["type"] == "auth_ok"


@pytest.mark.asyncio
async def test_authenticated_dashboard_client_has_no_deadline(
    gateway_ws_url, monkeypatch,
):
    """The bound covers only the pre-auth window: a subscriber sits idle for as
    long as it likes, waiting for events that may be minutes apart."""
    monkeypatch.setattr(ws_common, "DASHBOARD_AUTH_TIMEOUT_SECONDS", 0.3)
    dashboard_url = gateway_ws_url + "/dashboard"

    async with aiohttp.ClientSession() as s:
        async with s.ws_connect(dashboard_url) as ws:
            await ws.send_json({"type": "auth", "token": ""})
            assert (await asyncio.wait_for(ws.receive_json(), timeout=3))["type"] == "auth_ok"

            await asyncio.sleep(0.9)  # 3x the budget
            await ws.send_json({"type": "subscribe", "channels": ["cron"]})
            reply = await asyncio.wait_for(ws.receive_json(), timeout=3)
            assert reply["type"] == "subscribed"


def test_auth_deadline_reads_the_patched_timeout(monkeypatch):
    """Constructed budget follows the module attribute, so monkeypatching in
    tests takes effect instead of silently using the 10s production value."""
    monkeypatch.setattr(ws_common, "DASHBOARD_AUTH_TIMEOUT_SECONDS", 0.5)
    deadline = ws_common.AuthDeadline()
    remaining = deadline.remaining()
    assert remaining is not None and remaining <= 0.5


def test_auth_deadline_drops_the_bound_once_authenticated():
    deadline = ws_common.AuthDeadline(timeout_seconds=0.0)
    # Expired, but still bounded — never None, and never <= 0 (wait_for would
    # treat a non-positive timeout as scheduling-order dependent).
    assert deadline.remaining() == pytest.approx(0.001)
    deadline.mark_authenticated()
    assert deadline.remaining() is None


@pytest.mark.asyncio
async def test_shutdown_closes_authenticated_dashboard_sockets():
    """stop() must not wait on live dashboard handlers.

    It closed only its own _ws_clients registry, so an authenticated dashboard
    socket kept its handler running and runner.cleanup() blocked on it —
    shutdown could stall for aiohttp's shutdown timeout with a browser tab open.
    """
    from pathlib import Path
    from unittest.mock import AsyncMock, MagicMock

    from echo_agent.bus.queue import MessageBus
    from echo_agent.config.schema import (
        GatewayAuthConfig,
        GatewayConfig,
        GatewaySessionPolicyConfig,
    )
    from echo_agent.gateway.server import GatewayServer

    session_manager = MagicMock()
    session_manager.get_or_create = AsyncMock(return_value=MagicMock(status="active"))
    server = GatewayServer(
        config=GatewayConfig(
            enabled=True, host="127.0.0.1", port=0,
            auth=GatewayAuthConfig(mode="open", api_tokens=[]),
            session_policy=GatewaySessionPolicyConfig(mode="none"),
        ),
        bus=MessageBus(),
        channel_manager=MagicMock(),
        session_manager=session_manager,
        workspace=Path("/tmp/echo-agent-test-ws-shutdown"),
        agent_loop=None,
    )
    await server.start()
    url = f"ws://127.0.0.1:{server.actual_port}/ws/dashboard"

    async with aiohttp.ClientSession() as s:
        async with s.ws_connect(url) as ws:
            await ws.send_json({"type": "auth", "token": ""})
            assert (await asyncio.wait_for(ws.receive_json(), timeout=3))["type"] == "auth_ok"

            started = time.monotonic()
            await asyncio.wait_for(server.stop(), timeout=10)
            elapsed = time.monotonic() - started

            # Generous vs. aiohttp's ~60s shutdown wait, tight enough to fail if
            # cleanup blocks on the open handler.
            assert elapsed < 5, f"stop() took {elapsed:.1f}s with a dashboard client"
            # The client is told why, rather than seeing the socket vanish.
            msg = await asyncio.wait_for(ws.receive(), timeout=3)
            assert msg.type in (aiohttp.WSMsgType.CLOSE, aiohttp.WSMsgType.CLOSED)
