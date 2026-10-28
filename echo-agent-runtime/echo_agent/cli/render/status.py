"""Renderer-neutral vocabulary for the one-line CLI session status.

Both terminal front ends show the same facts but wrap them in different style
systems (prompt-toolkit fragments for inline, Textual markup for the TUI).  Keep
the measurements here so token, context and elapsed values cannot disagree
between the two views.
"""

from __future__ import annotations


# Shared by terminal renderers that need a compact braille activity indicator.
# Keeping the frames renderer-neutral lets prompt-toolkit own the interactive
# animation while the plain-output fallback can retain its existing ANSI
# implementation without the two drifting visually.
SPINNER_FRAMES = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"


def fmt_tokens(value: int | float | str | None) -> str:
    """Compact a token count without raising on optional telemetry."""
    try:
        number = max(0, int(value or 0))
    except (TypeError, ValueError):
        number = 0
    if number >= 1_000_000:
        return f"{number / 1_000_000:.1f}M"
    if number >= 1_000:
        return f"{number / 1_000:.1f}K"
    return str(number)


def fmt_duration(seconds: float | int | None) -> str:
    """Human duration for a status row (seconds, minutes, then hours)."""
    try:
        total = max(0, int(seconds or 0))
    except (TypeError, ValueError):
        total = 0
    if total < 60:
        return f"{total}s"
    minutes, remainder = divmod(total, 60)
    if minutes < 60:
        return f"{minutes}m {remainder}s" if remainder else f"{minutes}m"
    hours, minutes = divmod(minutes, 60)
    return f"{hours}h {minutes}m" if minutes else f"{hours}h"


def context_percent(used: int | None, maximum: int | None) -> int:
    """Clamp context occupancy to the range a gauge can display."""
    try:
        used_value = max(0, int(used or 0))
        max_value = int(maximum or 0)
    except (TypeError, ValueError):
        return 0
    if max_value <= 0:
        return 0
    return min(100, round(used_value / max_value * 100))


def context_gauge(percent: int, width: int = 10) -> str:
    """Return a fixed-width Unicode occupancy gauge."""
    clamped = max(0, min(100, int(percent)))
    size = max(1, int(width))
    filled = round(clamped / 100 * size)
    return "█" * filled + "░" * (size - filled)
