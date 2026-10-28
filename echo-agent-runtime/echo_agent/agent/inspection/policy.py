from __future__ import annotations

INSPECT_OK_SENTINEL = "INSPECT_OK"


def should_deliver(reply: str) -> bool:
    """False = silence (empty or sentinel present); True = deliver alert."""
    text = (reply or "").strip()
    if not text:
        return False
    if INSPECT_OK_SENTINEL in text:
        return False
    return True
