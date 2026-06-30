"""WS client session-key resolution — keeps the auth handshake's session
ownership check in a single, unit-testable place."""

from __future__ import annotations


def resolve_client_session_key(
    requested: str | None,
    *,
    platform: str,
    chat_id: str,
    allowed_prefixes: tuple[str, ...] = ("cli:",),
) -> tuple[str | None, str]:
    """Decide the session_key for a WS client.

    Empty/blank ``requested`` keeps the legacy behaviour (server-derived
    ``gateway:{platform}:{chat_id}``). A client-supplied key must start with
    one of ``allowed_prefixes`` so a thin client can only open its own ``cli:``
    session and never impersonate another channel's session (e.g.
    ``gateway:wechat:...``)."""
    fallback = f"gateway:{platform}:{chat_id}"
    if requested is None:
        return fallback, ""
    requested = requested.strip()
    if not requested:
        return fallback, ""
    if not any(requested.startswith(p) for p in allowed_prefixes):
        return None, f"session_key prefix not allowed: {requested!r}"
    return requested, ""
