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

    Epoch zero deliberately keeps the historical raw key.  Existing databases
    therefore remain readable until the first reset, while every later reset is
    isolated from all older episodes and plans without destructive deletion.
    """
    epoch = (
        session_epoch(session_or_epoch)
        if not isinstance(session_or_epoch, int)
        else max(0, session_or_epoch)
    )
    return session_key if epoch == 0 else f"{session_key}{_EPOCH_SEPARATOR}{epoch}"


def belongs_to_session(context_key: str, session_key: str) -> bool:
    """Whether a reset-bounded key belongs to the stable session key."""
    return context_key == session_key or context_key.startswith(
        f"{session_key}{_EPOCH_SEPARATOR}"
    )
