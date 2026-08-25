"""WS client session-key resolution — keeps the auth handshake's session
ownership check in a single, unit-testable place."""

from __future__ import annotations

# What a client gets when it reports a platform the gateway does not know. "ws"
# is the pre-existing default for a WS handshake that omits `platform`, so an
# unknown value lands on the same conservative channel as "didn't say".
UNKNOWN_PLATFORM_FALLBACK = "ws"


def normalize_platform(reported: str | None, known: list[str] | None) -> str:
    """Fold a client-reported ``platform`` onto a known value.

    ``platform`` is client-supplied on both the WS auth frame and POST /message,
    and it becomes part of ``channel="gateway:{platform}"``. Channel names carry
    capability decisions elsewhere — ``channels.stream_optimistic_channels``
    asserts "this channel can redraw text it already showed" — so an unconstrained
    value would let any caller claim a first-party client's capabilities and be
    served draft retractions it cannot honour.

    Unknown values fold to ``ws`` instead of being rejected: existing third-party
    callers post arbitrary platform strings and rejecting them buys no safety that
    the fold does not already provide. An empty/missing ``known`` list disables
    folding entirely, restoring the legacy fully-self-reported behaviour for
    anyone who needs it.

    This is deliberately NOT an identity control — two clients may both report
    ``desktop``. Impersonation is handled by :func:`resolve_client_session_key`.
    """
    reported = (reported or "").strip()
    if not reported:
        return UNKNOWN_PLATFORM_FALLBACK
    if not known:
        return reported
    return reported if reported in known else UNKNOWN_PLATFORM_FALLBACK


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
