# echo_agent/gateway/ws_dashboard.py
"""Dashboard WebSocket endpoint — real-time event subscription for UI clients.

Provides `/ws/dashboard` with token auth, channel subscribe/unsubscribe,
and a broadcast helper callable from other modules.
"""
from __future__ import annotations

import asyncio
import json
from typing import Any, TYPE_CHECKING

from aiohttp import web, WSMsgType

from echo_agent.gateway import ws_common

if TYPE_CHECKING:
    from echo_agent.gateway.server import GatewayServer


class DashboardWebSocket:
    def __init__(self, server: GatewayServer):
        self._server = server
        self._clients: dict[str, _DashboardClient] = {}
        self._counter = 0
        self._unauthenticated = 0

    async def handle(self, request: web.Request) -> web.StreamResponse:
        """Origin gate → upgrade → authenticated handshake → message loop.

        The gate runs before prepare(): a cross-site page whose socket has been
        upgraded already sees onopen succeed, so refusing inside the loop leaks
        the fact that the endpoint exists and holds a connection slot.
        """
        rejected = ws_common.reject_cross_site(request, self._server.auth, action="dashboard_ws_auth")
        if rejected is not None:
            return rejected

        if self._unauthenticated >= ws_common.MAX_UNAUTHENTICATED_CLIENTS:
            self._server.auth.audit(
                "dashboard_ws_auth", ok=False,
                reason=f"too many unauthenticated clients ({self._unauthenticated})",
            )
            return web.json_response({"error": "too many unauthenticated connections"}, status=503)

        # Same server-driven heartbeat as the main WS: without it a client on a
        # stalled TCP connection dies unnoticed and keeps receiving nothing.
        hb = getattr(self._server._config, "ws_heartbeat_seconds", 0)
        ws = web.WebSocketResponse(heartbeat=hb if hb and hb > 0 else None)
        await ws.prepare(request)

        client_id = f"dash_{self._counter}"
        self._counter += 1
        client = _DashboardClient(client_id, ws)
        authenticated = False
        self._unauthenticated += 1

        try:
            while True:
                try:
                    # Bound the pre-auth wait only. Once authenticated the client
                    # is a legitimate long-lived subscriber and must be able to
                    # sit idle between events.
                    timeout = None if authenticated else ws_common.DASHBOARD_AUTH_TIMEOUT_SECONDS
                    msg = await asyncio.wait_for(ws.receive(), timeout=timeout)
                except asyncio.TimeoutError:
                    self._server.auth.audit(
                        "dashboard_ws_auth", ok=False, reason="authentication timeout"
                    )
                    await ws.close()
                    break

                if msg.type != WSMsgType.TEXT:
                    if msg.type in (WSMsgType.ERROR, WSMsgType.CLOSE, WSMsgType.CLOSED,
                                    WSMsgType.CLOSING):
                        break
                    continue

                try:
                    data = json.loads(msg.data)
                except json.JSONDecodeError:
                    continue

                msg_type = data.get("type")

                if msg_type == "auth":
                    token = data.get("token", "")
                    if self._server.auth.authenticate_token(token):
                        if not authenticated:
                            authenticated = True
                            self._unauthenticated -= 1
                        self._clients[client_id] = client
                        self._server.auth.audit("dashboard_ws_auth", ok=True)
                        await ws.send_json({"type": "auth_ok"})
                    else:
                        self._server.auth.audit(
                            "dashboard_ws_auth", ok=False, reason="invalid token"
                        )
                        await ws.send_json({"type": "auth_error", "message": "invalid token"})
                        await ws.close()
                        break

                elif not authenticated:
                    await ws.send_json({"type": "error", "message": "not authenticated"})

                elif msg_type == "subscribe":
                    requested = [str(c) for c in (data.get("channels") or [])]
                    known = set(self._EVENT_CHANNEL_MAP.values())
                    accepted = [c for c in requested if c in known]
                    unknown = [c for c in requested if c not in known]
                    client.subscriptions.update(accepted)
                    if unknown:
                        # Silently accepting an unknown channel produced a
                        # subscription that could never deliver anything, which
                        # the UI could not distinguish from an idle one.
                        await ws.send_json({
                            "type": "subscribe_error",
                            "channels": sorted(accepted),
                            "unknown": unknown,
                            "message": "unknown channel(s)",
                        })
                    else:
                        await ws.send_json({
                            "type": "subscribed",
                            "channels": sorted(client.subscriptions),
                        })

                elif msg_type == "unsubscribe":
                    channels = [str(c) for c in (data.get("channels") or [])]
                    client.subscriptions -= set(channels)
                    await ws.send_json({"type": "unsubscribed", "channels": channels})
        finally:
            if not authenticated:
                self._unauthenticated -= 1
            self._clients.pop(client_id, None)

        return ws

    # Event prefix → subscription channel. `tasks` (tasks.manager) and `cron`
    # (scheduler.service) are the two with a live sink wired in app.py; the rest
    # are reserved names that nothing emits into yet, so subscribing to them
    # from the UI silently yields no events. Keep them declared so a future
    # emitter routes correctly without a protocol change, but do not treat their
    # presence as a signal that the channel is live.
    _EVENT_CHANNEL_MAP: dict[str, str] = {
        "task": "tasks",
        "cron": "cron",
        # --- reserved, no emitter yet ---
        "session": "sessions",
        "memory": "memory",
        "skill": "skills",
        "channel": "channels",
        "log": "logs",
        "analytics": "analytics",
        "knowledge": "knowledge",
    }

    async def broadcast(self, event_type: str, payload: dict[str, Any], channel: str | None = None) -> None:
        """Broadcast an event to all subscribed dashboard clients.

        Channel is resolved from a static mapping of event prefix → subscription
        channel name, unless explicitly provided.
        """
        if channel:
            ch = channel
        else:
            prefix = event_type.split("_")[0] if "_" in event_type else event_type
            ch = self._EVENT_CHANNEL_MAP.get(prefix, prefix)
        message = json.dumps({"type": event_type, "payload": payload})
        dead: list[str] = []
        for cid, client in list(self._clients.items()):
            if ch in client.subscriptions:
                try:
                    await client.ws.send_str(message)
                except Exception:
                    dead.append(cid)
        for cid in dead:
            self._clients.pop(cid, None)


class _DashboardClient:
    __slots__ = ("id", "ws", "subscriptions")

    def __init__(self, client_id: str, ws: web.WebSocketResponse):
        self.id = client_id
        self.ws = ws
        self.subscriptions: set[str] = set()
