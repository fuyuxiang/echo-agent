"""WS client session-key resolution — keeps the auth handshake's session
ownership check in a single, unit-testable place."""

from __future__ import annotations


def resolve_client_session_key(
    requested: str | None,
    *,
    platform: str,
    chat_id: str,
    allow_fallback: bool = True,
    allowed_prefixes: tuple[str, ...] = ("cli:",),
) -> tuple[str | None, str]:
    """Decide the session_key for a WS client.

    A client-supplied key must start with one of ``allowed_prefixes`` so a thin
    client can only open its own ``cli:`` session, never impersonate another
    channel (e.g. ``gateway:wechat:...``).

    ``allow_fallback`` gates the legacy server-derived key
    ``gateway:{platform}:{chat_id}``. It is ``True`` for normally-authorized
    clients (open mode / allowlisted). It is ``False`` when the client was let
    in *only* by the loopback exemption: such a client must present an explicit
    ``cli:`` key and is otherwise rejected, so a bare loopback caller cannot
    self-report ``platform=wechat, user_id=victim`` and land on another user's
    ``gateway:wechat:victim`` session."""
    fallback = f"gateway:{platform}:{chat_id}"
    if requested is not None:
        requested = requested.strip()
    if not requested:
        if allow_fallback:
            return fallback, ""
        return None, "session_key required for loopback-only client"
    if not any(requested.startswith(p) for p in allowed_prefixes):
        return None, f"session_key prefix not allowed: {requested!r}"
    return requested, ""
