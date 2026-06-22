from __future__ import annotations

from typing import TYPE_CHECKING

from aiohttp import web

if TYPE_CHECKING:
    from echo_agent.gateway.server import GatewayServer


class LifecycleAPI:
    def __init__(self, server: GatewayServer):
        self._server = server

    def _guard(self, request: web.Request, action: str) -> web.Response | None:
        return self._server._require_api_token(request, action=action)

    async def shutdown(self, request: web.Request) -> web.Response:
        # Shutdown is a high-risk admin action — require an admin-scoped token.
        guard = self._server._require_admin_token(request, action="shutdown")
        if guard is not None:
            return guard

        if not self._server._shutdown_event:
            return web.json_response({
                "error": "shutdown not available",
                "message": "Gateway was not started with lifecycle management support",
            }, status=503)

        self._server.request_shutdown()

        return web.json_response({
            "status": "shutting_down",
            "drain_timeout_seconds": 10,
            "message": "Agent will shut down after draining in-flight requests (max 10s)",
        }, status=202)

    async def health(self, request: web.Request) -> web.Response:
        """Extended health endpoint for lifecycle management."""
        data = await self._server.health.check()
        status_code = 200 if data["status"] != "unhealthy" else 503
        return web.json_response(data, status=status_code)
