"""Conversation-epoch helpers.

``session_key`` identifies the durable conversation owner and delivery route.
It is intentionally stable (the CLI always reconnects as ``cli:<user>``), so it
cannot also identify one reset-bounded conversation.  ``reset_count`` already
lives in Session.metadata and is persisted atomically with a reset; use it as
the epoch instead of inventing a second counter that can drift.
"""

from __future__ import annotations

from typing import Any


_EPOCH_SEPARATOR = "::epoch:"
_STRUCTURED_CONTEXT_PREFIX = "::context:v1:"


def has_reserved_context_syntax(session_key: str) -> bool:
    """Whether *session_key* overlaps the internal context-key grammar.

    Gateway clients must never supply such a key directly.  The second check
    reserves the escape envelope as well as the historical epoch separator so
    future encodings cannot be confused with a raw session identity.
    """
    return (
        _EPOCH_SEPARATOR in session_key
        or session_key.startswith(_STRUCTURED_CONTEXT_PREFIX)
    )


def _structured_context_prefix(session_key: str) -> str:
    """Injectively frame an otherwise ambiguous session-key component.

    A length prefix, rather than another delimiter-only join, makes arbitrary
    Unicode session keys unambiguous.  The envelope itself is reserved by
    :func:`has_reserved_context_syntax`, so a raw key cannot impersonate it.
    """
    return f"{_STRUCTURED_CONTEXT_PREFIX}{len(session_key)}:{session_key}{_EPOCH_SEPARATOR}"


def _is_canonical_epoch(value: str, *, allow_zero: bool) -> bool:
    if not value or not value.isascii() or not value.isdigit():
        return False
    epoch = int(value)
    return epoch >= (0 if allow_zero else 1) and value == str(epoch)


def session_epoch(session: Any) -> int:
    """Return a non-negative persisted epoch for *session*."""
    metadata = getattr(session, "metadata", None)
    if not isinstance(metadata, dict):
        return 0
    try:
        return max(0, int(metadata.get("reset_count", 0)))
    except (TypeError, ValueError):
        return 0


def conversation_context_key(session_key: str, session_or_epoch: Any = 0) -> str:
    """Return the key for reset-bounded prompt/task state.

    Epoch zero deliberately keeps the historical raw key *when it is
    unambiguous*.  Existing databases therefore remain readable for ordinary
    session keys.  A key containing reserved syntax is instead length-framed,
    which makes ``(session_key, epoch)`` injective even for trusted/internal
    callers that did not pass through the Gateway validator.
    """
    epoch = (
        session_epoch(session_or_epoch)
        if not isinstance(session_or_epoch, int)
        else max(0, int(session_or_epoch))
    )
    if has_reserved_context_syntax(session_key):
        return f"{_structured_context_prefix(session_key)}{epoch}"
    return session_key if epoch == 0 else f"{session_key}{_EPOCH_SEPARATOR}{epoch}"


def belongs_to_session(context_key: str, session_key: str) -> bool:
    """Whether a reset-bounded key belongs to the stable session key."""
    if has_reserved_context_syntax(session_key):
        prefix = _structured_context_prefix(session_key)
        return context_key.startswith(prefix) and _is_canonical_epoch(
            context_key[len(prefix):], allow_zero=True,
        )

    if context_key == session_key:
        return True
    prefix = f"{session_key}{_EPOCH_SEPARATOR}"
    return context_key.startswith(prefix) and _is_canonical_epoch(
        context_key[len(prefix):], allow_zero=False,
    )
