from __future__ import annotations

from typing import TYPE_CHECKING

from aiohttp import web

if TYPE_CHECKING:
    from echo_agent.gateway.server import GatewayServer


class AnalyticsAPI:
    def __init__(self, server: GatewayServer):
        self._server = server

    def _guard(self, request: web.Request, action: str) -> web.Response | None:
        return self._server._require_api_token(request, action=action)

    @staticmethod
    def _int_param(request: web.Request, name: str, default: int) -> int | None:
        raw = request.query.get(name, str(default))
        try:
            return int(raw)
        except (ValueError, TypeError):
            return None

    async def token_usage(self, request: web.Request) -> web.Response:
        guard = self._guard(request, "analytics_tokens")
        if guard is not None:
            return guard

        days = self._int_param(request, "days", 7)
        if days is None:
            return web.json_response({"error": "invalid 'days' parameter"}, status=400)
        usage = await self._server._agent_loop.cost_tracker.get_daily_usage(days=days)
        return web.json_response({"usage": usage, "days": days})

    async def skill_usage(self, request: web.Request) -> web.Response:
        guard = self._guard(request, "analytics_skills")
        if guard is not None:
            return guard

        days = self._int_param(request, "days", 7)
        if days is None:
            return web.json_response({"error": "invalid 'days' parameter"}, status=400)
        skills = await self._server._agent_loop.cost_tracker.get_skill_usage(days=days)
        return web.json_response({"skills": skills})

    async def channel_usage(self, request: web.Request) -> web.Response:
        guard = self._guard(request, "analytics_channels")
        if guard is not None:
            return guard

        days = self._int_param(request, "days", 7)
        if days is None:
            return web.json_response({"error": "invalid 'days' parameter"}, status=400)
        channels = await self._server._agent_loop.cost_tracker.get_channel_usage(days=days)
        return web.json_response({"channels": channels})
