from __future__ import annotations

from typing import TYPE_CHECKING

from aiohttp import web

if TYPE_CHECKING:
    from echo_agent.gateway.server import GatewayServer


class SessionsAPI:
    def __init__(self, server: GatewayServer):
        self._server = server

    def _guard(self, request: web.Request, action: str) -> web.Response | None:
        return self._server._require_api_token(request, action=action)

    async def list_sessions(self, request: web.Request) -> web.Response:
        guard = self._guard(request, "sessions_list")
        if guard is not None:
            return guard

        channel = request.query.get("channel")
        sessions = await self._server.session_manager.list_sessions()

        if channel:
            sessions = [s for s in sessions if s.get("key", "").startswith(channel)]

        return web.json_response({"sessions": sessions, "total": len(sessions)})

    async def get_history(self, request: web.Request) -> web.Response:
        guard = self._guard(request, "sessions_history")
        if guard is not None:
            return guard

        key = request.match_info["key"]
        try:
            limit = int(request.query.get("limit", "100"))
        except (ValueError, TypeError):
            return web.json_response({"error": "invalid limit parameter"}, status=400)

        session = await self._server.session_manager.get_or_create(key)
        messages = session.get_history(max_messages=limit)

        return web.json_response({"messages": messages, "total": len(messages)})
