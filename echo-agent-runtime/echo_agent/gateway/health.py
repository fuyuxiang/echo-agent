"""Gateway health provider — reports gateway subsystem status."""

from __future__ import annotations

from datetime import datetime, timedelta
from typing import Any, TYPE_CHECKING

if TYPE_CHECKING:
    from echo_agent.gateway.server import GatewayServer


# A session counts as "active" if it was touched within this window. Without a
# window, ``active_sessions`` was simply the total number of sessions ever
# persisted: a monotonically growing number that no longer answered the question
# the dashboard card asks ("how much is going on right now?"). 15 minutes is long
# enough to span a user thinking between turns, short enough that an idle
# deployment reports zero.
ACTIVE_SESSION_WINDOW = timedelta(minutes=15)


def _count_recently_active(sessions: list[dict[str, Any]]) -> int:
    """Sessions whose ``updated_at`` falls inside ACTIVE_SESSION_WINDOW.

    Timestamps come from storage as ISO strings (see SessionManager); anything
    unparseable is treated as *not* active rather than raising — this feeds a
    health check that must never fail on a malformed row."""
    cutoff = datetime.now() - ACTIVE_SESSION_WINDOW
    count = 0
    for session in sessions:
        raw = session.get("updated_at")
        if not raw:
            continue
        try:
            updated = datetime.fromisoformat(str(raw))
        except (TypeError, ValueError):
            continue
        # Compare naive-to-naive: storage writes local naive isoformat, so an
        # aware value (from a differently-configured writer) is normalised to
        # local time first instead of raising on the comparison.
        if updated.tzinfo is not None:
            updated = updated.astimezone().replace(tzinfo=None)
        if updated >= cutoff:
            count += 1
    return count


class GatewayHealthProvider:

    def __init__(self, gateway: GatewayServer):
        self._gw = gateway

    def readiness(self) -> dict[str, Any]:
        """Local task-admission readiness, without paid remote model probes.

        A listening configuration UI is useful even with no model configured.
        Keep that liveness separate from the runtime's ability to accept work.
        Optional integrations do not block readiness; the existing execution
        guard owns failures of required runtime extensions.
        """
        agent = self._gw._agent_loop
        provider = getattr(agent, "provider", None)
        storage = getattr(agent, "_storage", None)
        checks = {
            "gateway": {"ok": self._gw.is_running, "reason": "gateway is not listening"},
            "bus": {"ok": getattr(self._gw._bus, "_running", False) is True, "reason": "message bus is not running"},
            "runtime": {"ok": getattr(agent, "is_running", False) is True, "reason": "agent runtime is not running"},
            "storage": {"ok": getattr(storage, "is_connected", False) is True, "reason": "persistent storage is disconnected"},
            "provider": {"ok": provider is not None and getattr(provider, "is_stub", False) is not True,
                         "reason": "configure a real model provider and restart the runtime"},
            "required_dependencies": {"ok": agent is not None, "reason": "agent runtime is unavailable"},
        }
        guard = getattr(agent, "_assert_runtime_healthy", None)
        if callable(guard):
            try:
                guard()
            except Exception:
                # Required extension failures can contain endpoint details.
                # Keep this unauthenticated probe free of secrets and logs.
                checks["required_dependencies"] = {
                    "ok": False, "reason": "a required runtime dependency failed; inspect the service log",
                }
        for check in checks.values():
            if check["ok"]:
                check["reason"] = "ok"
        ready = all(check["ok"] for check in checks.values())
        return {
            "ready": ready, "status": "ready" if ready else "not_ready", "checks": checks,
            "provider_remote_verified": False,
        }

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

        # Interactive WS clients (TUI/playground), distinct from standing channels.
        ws_client_count = 0
        try:
            clients = getattr(self._gw, "_ws_clients", None)
            if clients is not None:
                ws_client_count = len(clients)
        except TypeError:
            ws_client_count = 0

        session_count = 0
        active_session_count = 0
        if self._gw.session_manager:
            list_sessions = getattr(self._gw.session_manager, "list_sessions_async", None)
            sessions = await list_sessions() if list_sessions else self._gw.session_manager.list_sessions()
            session_count = len(sessions)
            active_session_count = _count_recently_active(sessions)

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
            "ws_clients": ws_client_count,
            "provider": provider_status,
            "rate_limiter": rate_stats,
            "media_cache_mb": round(media_size, 1),
            "hooks_loaded": self._gw.hooks.handler_count if self._gw.hooks else 0,
            "delivery_rules": self._gw.delivery_router.rule_count if self._gw.delivery_router else 0,
            # Sessions touched within ACTIVE_SESSION_WINDOW. ``active_sessions``
            # used to carry the all-time total, so the dashboard's "active
            # sessions" card only ever went up; the total is still reported
            # separately as ``total_sessions``.
            "active_sessions": active_session_count,
            "total_sessions": session_count,
        }
