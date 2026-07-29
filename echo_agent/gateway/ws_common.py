# echo_agent/gateway/ws_common.py
"""Shared WebSocket handshake guards.

Both WS endpoints need the same pre-upgrade cross-site gate and the same bound
on how long a socket may stay unauthenticated. The main WS grew its Origin gate
first; the dashboard WS had none and authenticated inside the message loop,
which is after `prepare()` — by then the browser's onopen has fired and a
cross-site page holds a live socket. Keeping both gates in one place is what
stops the two endpoints from drifting apart again.
"""
from __future__ import annotations

from typing import Any

from aiohttp import web

# How long a socket may stay unauthenticated before it is closed. Neither
# endpoint had this: an idle pre-auth socket held a connection slot forever.
DASHBOARD_AUTH_TIMEOUT_SECONDS = 10.0

# Ceiling on concurrent unauthenticated sockets. Authenticated clients are not
# counted — this bounds only the pre-auth window, which is the part an anonymous
# caller controls.
MAX_UNAUTHENTICATED_CLIENTS = 8


def reject_cross_site(request: web.Request, auth: Any, *, action: str) -> web.Response | None:
    """Refuse cross-site browser upgrades BEFORE prepare().

    Returns a 403 response to return from the handler, or None to continue.
    Non-browser clients (the CLI) send no Origin / Sec-Fetch-Site and pass.
    """
    origin = request.headers.get("Origin", "").strip()
    sec_fetch_site = request.headers.get("Sec-Fetch-Site", "").strip()
    if not auth.is_cross_site_browser(origin, sec_fetch_site):
        return None
    auth.audit(action, ok=False, reason=f"cross-site origin rejected: {origin or '?'}")
    return web.json_response({"error": "cross-site request forbidden"}, status=403)
