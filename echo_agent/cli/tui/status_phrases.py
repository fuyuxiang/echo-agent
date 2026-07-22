"""Rotating status phrases for the waiting/heartbeat line.

Instead of echoing the
raw server-supplied note (which is monotonous and can leak model scratch text),
pick a short, friendly phrase and avoid repeating the last few. Pure logic — no
Textual/screen dependency — so it is unit-testable in isolation.
"""

from __future__ import annotations

import random as _random
from collections.abc import MutableSequence
from typing import Any

# Kept deliberately short and generic. These are UI reassurance, not progress
# detail — the real progress lives in the tool/thinking blocks above the line.
_PHRASES: tuple[str, ...] = (
    "还在处理",
    "正在思考",
    "马上就好",
    "继续推进中",
    "还在忙这个",
    "梳理一下思路",
    "正在整理结果",
    "稍等片刻",
    "还在跑",
    "接着往下做",
)

# How many recent picks to remember so we don't repeat ourselves.
_RECENT_WINDOW = 4


def choose_status_phrase(
    recent: MutableSequence[str] | None = None,
    *,
    rng: Any = None,
    phrases: tuple[str, ...] = _PHRASES,
) -> str:
    """Pick a phrase, avoiding the ones in ``recent``.

    ``recent`` (if given) is mutated in place: the chosen phrase is appended and
    the list is trimmed to the most recent ``_RECENT_WINDOW`` entries, so the
    caller can keep a single rolling window across calls.
    """
    candidates = list(phrases)
    if recent:
        fresh = [p for p in candidates if p not in set(recent)]
        if fresh:
            candidates = fresh
    picker = rng or _random
    phrase = picker.choice(candidates)
    if recent is not None:
        recent.append(phrase)
        del recent[:-_RECENT_WINDOW]
    return phrase
