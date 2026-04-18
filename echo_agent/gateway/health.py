"""Gateway health provider — reports gateway subsystem status."""

from __future__ import annotations

from typing import Any, TYPE_CHECKING

if TYPE_CHECKING:
    from echo_agent.gateway.server import GatewayServer


class GatewayHealthProvider:

    def __init__(self, gateway: GatewayServer):
        self._gw = gateway

    async def check(self) -> dict[str, Any]:
        is_running = self._gw.is_running

        channel_status = {}
        if self._gw.channel_manager:
            for name in self._gw.channel_manager.active_channels:
                channel_status[name] = "active"

        rate_stats = {}
        if self._gw.rate_limiter:
            rate_stats = self._gw.rate_limiter.get_stats()

        media_size = 0.0
        if self._gw.media_cache:
            media_size = self._gw.media_cache.get_size_mb()

        session_count = 0
        if self._gw.session_manager:
            session_count = len(self._gw.session_manager.list_sessions())

        status = "healthy" if is_running else "unhealthy"
        if is_running and not channel_status:
            status = "degraded"

        return {
            "status": status,
            "server_running": is_running,
            "active_channels": channel_status,
            "rate_limiter": rate_stats,
            "media_cache_mb": round(media_size, 1),
            "active_sessions": session_count,
            "hooks_loaded": self._gw.hooks.handler_count if self._gw.hooks else 0,
            "delivery_rules": self._gw.delivery_router.rule_count if self._gw.delivery_router else 0,
        }
