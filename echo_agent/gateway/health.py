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

        # Number of interactive WebSocket clients currently attached to /ws (the
        # TUI, web playground). Distinct from channels: these are transient
        # consumers, not standing delivery paths. A count ≥1 means at least one
        # CLI/TUI is connected — enough for the ops-view "is the CLI attached?"
        # check without modeling per-client identity. Defensive: never let this
        # metric break the health check itself.
        ws_client_count = 0
        try:
            clients = getattr(self._gw, "_ws_clients", None)
            if clients is not None:
                ws_client_count = len(clients)
        except TypeError:
            ws_client_count = 0

        session_count = 0
        if self._gw.session_manager:
            list_sessions = getattr(self._gw.session_manager, "list_sessions_async", None)
            sessions = await list_sessions() if list_sessions else self._gw.session_manager.list_sessions()
            session_count = len(sessions)

        status = "healthy" if is_running else "unhealthy"
        if is_running and not channel_status:
            status = "degraded"

        provider_status = "ok"
        if self._gw._agent_loop:
            provider = getattr(self._gw._agent_loop, 'provider', None)
            if provider and getattr(provider, 'is_stub', False) is True:
                status = "degraded"
                provider_status = "stub"

        return {
            "status": status,
            "server_running": is_running,
            "active_channels": channel_status,
            # Number of interactive WebSocket clients currently attached to /ws
            # (the TUI, web playground). Distinct from channels: these are
            # transient consumers, not standing delivery paths. A count ≥1 means
            # at least one CLI/TUI is connected — enough for the ops-view check
            # of "is the CLI attached?" without modeling per-client identity.
            "ws_clients": ws_client_count,
            "provider": provider_status,
            "rate_limiter": rate_stats,
            "media_cache_mb": round(media_size, 1),
            "active_sessions": session_count,
            "hooks_loaded": self._gw.hooks.handler_count if self._gw.hooks else 0,
            "delivery_rules": self._gw.delivery_router.rule_count if self._gw.delivery_router else 0,
        }
