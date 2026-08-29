"""24-bit ANSI painting for the inline renderer.

Colour policy is NOT reimplemented here: cli/colors.py already resolves the
three-way decision (explicit override > NO_COLOR > isatty), and the setup
wizard and every --json path already drive it. This module adds the palette
lookup and a paint() that degrades to plain text, so a piped or NO_COLOR run
emits no escapes at all.
"""

from __future__ import annotations

from echo_agent.cli.colors import (
    color_enabled, set_color_override as set_color_override,
)
from echo_agent.cli.palette import active_palette

RESET = "\033[0m"
BOLD = "\033[1m"
DIM = "\033[2m"

# Palette is resolved once: light/dark detection reads the environment, which
# does not change mid-process, and rebuilding the dict per painted line showed
# up in profiles of tool-heavy turns.
_PALETTE: dict[str, str] | None = None


def _palette() -> dict[str, str]:
    global _PALETTE
    if _PALETTE is None:
        _PALETTE = active_palette()
    return _PALETTE


def reset_palette_cache() -> None:
    """Drop the cached palette so the next fg() re-resolves it.

    /theme switches the light/dark palette at runtime, and the cache would
    otherwise keep serving the palette resolved at first paint. Exposed as a
    function rather than letting callers assign to the module global: the
    private name is not a contract, and a hook is where a future validation or
    invalidation-logging step belongs.
    """
    global _PALETTE
    _PALETTE = None


def supports_color() -> bool:
    """Whether ANSI escapes should be emitted, per cli/colors.py policy."""
    return color_enabled()


def fg(role: str) -> str:
    """24-bit foreground escape for a named palette role.

    Raises KeyError for an unknown role rather than returning an empty string:
    a typo'd role is a bug in the caller, and silently emitting uncoloured text
    would hide it.
    """
    value = _palette().get(role)
    if value is None:
        raise KeyError(f"Unknown palette role: {role}")
    red, green, blue = (int(value[i:i + 2], 16) for i in (1, 3, 5))
    return f"\033[38;2;{red};{green};{blue}m"


def paint(text: str, *codes: str) -> str:
    """Wrap text in the given escapes, or return it unchanged when colour is off."""
    if not codes or not supports_color():
        return text
    return "".join(codes) + text + RESET
