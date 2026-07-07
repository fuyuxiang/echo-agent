from __future__ import annotations

from typing import TYPE_CHECKING

from aiohttp import web

if TYPE_CHECKING:
    from echo_agent.gateway.server import GatewayServer


class LogsAPI:
    def __init__(self, server: GatewayServer):
        self._server = server

    def _guard(self, request: web.Request, action: str) -> web.Response | None:
        return self._server._require_api_token(request, action=action)

    async def list_logs(self, request: web.Request) -> web.Response:
        guard = self._guard(request, "logs_list")
        if guard is not None:
            return guard

        level = request.query.get("level")
        search = request.query.get("q")
        try:
            limit = int(request.query.get("limit", "200"))
            offset = int(request.query.get("offset", "0"))
        except (ValueError, TypeError):
            return web.json_response({"error": "invalid limit/offset parameter"}, status=400)

        logs = list(self._server._agent_loop.log_buffer)

        if level:
            logs = [entry for entry in logs if entry.get("level") == level.upper()]
        if search:
            logs = [entry for entry in logs if search.lower() in entry.get("message", "").lower()]

        total = len(logs)
        logs = logs[offset:offset + limit]

        return web.json_response({"logs": logs, "total": total})
