# echo_agent/gateway/ws_dashboard.py
"""Dashboard WebSocket endpoint — real-time event subscription for UI clients.

Provides `/ws/dashboard` with token auth, channel subscribe/unsubscribe,
and a broadcast helper callable from other modules.
"""
from __future__ import annotations

import json
from typing import Any, TYPE_CHECKING

from aiohttp import web, WSMsgType

if TYPE_CHECKING:
    from echo_agent.gateway.server import GatewayServer


class DashboardWebSocket:
    def __init__(self, server: GatewayServer):
        self._server = server
        self._clients: dict[str, _DashboardClient] = {}
        self._counter = 0

    async def handle(self, request: web.Request) -> web.WebSocketResponse:
        ws = web.WebSocketResponse()
        await ws.prepare(request)

        client_id = f"dash_{self._counter}"
        self._counter += 1
        client = _DashboardClient(client_id, ws)
        authenticated = False

        try:
            async for msg in ws:
                if msg.type == WSMsgType.TEXT:
                    try:
                        data = json.loads(msg.data)
                    except json.JSONDecodeError:
                        continue

                    if data.get("type") == "auth":
                        token = data.get("token", "")
                        if self._server.auth.authenticate_token(token):
                            authenticated = True
                            self._clients[client_id] = client
                            await ws.send_json({"type": "auth_ok"})
                        else:
                            await ws.send_json({"type": "auth_error", "message": "invalid token"})
                            await ws.close()
                            return ws

                    elif not authenticated:
                        await ws.send_json({"type": "error", "message": "not authenticated"})

                    elif data.get("type") == "subscribe":
                        channels = data.get("channels", [])
                        client.subscriptions.update(channels)
                        await ws.send_json({"type": "subscribed", "channels": list(client.subscriptions)})

                    elif data.get("type") == "unsubscribe":
                        channels = data.get("channels", [])
                        client.subscriptions -= set(channels)
                        await ws.send_json({"type": "unsubscribed", "channels": channels})

                elif msg.type in (WSMsgType.ERROR, WSMsgType.CLOSE):
                    break
        finally:
            self._clients.pop(client_id, None)

        return ws

    _EVENT_CHANNEL_MAP: dict[str, str] = {
        "task": "tasks",
        "session": "sessions",
        "cron": "cron",
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
