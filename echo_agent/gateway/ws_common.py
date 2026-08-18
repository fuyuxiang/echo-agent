"""Shared WebSocket handshake guards.

Both WS endpoints need the same pre-upgrade cross-site gate and the same bound
on how long a socket may stay unauthenticated. The main WS grew its Origin gate
first; the dashboard WS had none and authenticated inside the message loop,
which is after `prepare()` — by then the browser's onopen has fired and a
cross-site page holds a live socket. Keeping both gates in one place is what
stops the two endpoints from drifting apart again.
"""
from __future__ import annotations

import time
from typing import Any

from aiohttp import web

# How long a socket may stay unauthenticated before it is closed. Neither
# endpoint had this: an idle pre-auth socket held a connection slot forever.
DASHBOARD_AUTH_TIMEOUT_SECONDS = 10.0


class AuthDeadline:
    """An absolute bound on the pre-auth window, not a per-frame idle timeout.

    Both endpoints authenticate inside their message loop and wrapped each
    ``receive()`` in ``wait_for(..., timeout=AUTH_TIMEOUT)``. That timer restarts
    on every frame, so the bound it enforces is "10s of silence" rather than "10s
    to authenticate": a peer that keeps sending frames it is not entitled to send
    — junk, unparseable JSON, `message` before `auth` — renews its own deadline
    indefinitely and keeps holding one of the limited unauthenticated slots.

    Computing the remaining budget from a fixed start instant makes the bound
    absolute regardless of traffic. ``remaining()`` returns None once
    authenticated, because an authenticated client is a legitimate long-lived
    one: a TUI turn can run for many minutes with no client frames.
    """

    __slots__ = ("_expires_at", "authenticated")

    def __init__(self, timeout_seconds: float | None = None):
        # Read the module attribute at construction so tests that monkeypatch
        # DASHBOARD_AUTH_TIMEOUT_SECONDS still take effect.
        budget = (
            DASHBOARD_AUTH_TIMEOUT_SECONDS
            if timeout_seconds is None
            else timeout_seconds
        )
        self._expires_at = time.monotonic() + budget
        self.authenticated = False

    def mark_authenticated(self) -> None:
        self.authenticated = True

    def remaining(self) -> float | None:
        """Seconds left to authenticate, or None when no bound applies.

        Never returns a value <= 0: wait_for treats those as "already expired"
        but only after scheduling, so a tiny positive floor keeps the timeout
        path deterministic rather than depending on loop scheduling order.
        """
        if self.authenticated:
            return None
        return max(0.001, self._expires_at - time.monotonic())

# Ceiling on concurrent unauthenticated sockets. Authenticated clients are not
# counted — this bounds only the pre-auth window, which is the part an anonymous
# caller controls.
MAX_UNAUTHENTICATED_CLIENTS = 8


def reject_cross_site(request: web.Request, auth: Any, *, action: str) -> web.Response | None:
    """Refuse cross-site browser upgrades BEFORE prepare().

    Returns a 403 response to return from the handler, or None to continue.
    Non-browser clients (the CLI) send no Origin / Sec-Fetch-Site and pass.

    Two independent gates apply: the cross-site Origin/Sec-Fetch-Site check
    (CSRF primitive), and the Host header must name a host this gateway was
    intended to be reached on. The second one is what closes DNS rebinding:
    with the first alone, a page whose DNS has been rebound to 127.0.0.1 sends
    Sec-Fetch-Site=same-origin and Host=evil.example, both consistent with its
    own page origin — and is let through.

    Order matters. ``is_cross_site_browser`` returns False for *any*
    non-browser request AND for legitimate same-origin browser requests; the
    host check must run on both, since a same-origin rebound IS still a
    browser request. So we identify browser requests first (by carrying an
    Origin or a non-trivial Sec-Fetch-Site), then apply both gates. A request
    that carries neither is non-browser and short-circuits.
    """
    origin = request.headers.get("Origin", "").strip()
    sec_fetch_site = request.headers.get("Sec-Fetch-Site", "").strip()
    host = request.headers.get("Host", "").strip()
    is_browser_request = bool(origin) or sec_fetch_site not in ("", "none")
    if not is_browser_request:
        return None
    # Browser request. Both gates must pass independently — same-origin alone
    # is not enough (rebind), cross-site Origin alone is not enough (the
    # allowlist claim), Host alone is not enough (proxy header forgery).
    if auth.is_cross_site_browser(origin, sec_fetch_site, host):
        auth.audit(action, ok=False, reason=f"cross-site origin rejected: {origin or '?'}")
        return web.json_response({"error": "cross-site request forbidden"}, status=403)
    if not auth.is_host_allowed(host):
        auth.audit(action, ok=False, reason=f"untrusted host rejected: {host or '?'}")
        return web.json_response({"error": "cross-site request forbidden"}, status=403)
    return None
