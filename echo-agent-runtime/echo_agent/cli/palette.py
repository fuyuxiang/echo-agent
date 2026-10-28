"""Shared terminal palette for the classic CLI and the Textual TUI.

This module deliberately has no optional UI dependencies. The setup wizard
can therefore use the same colors and light/dark detection as the TUI even
when the ``tui`` extra is not installed.
"""

from __future__ import annotations

import os
from collections.abc import Mapping


DARK_PALETTE = {
    "primary": "#4fd1c5",
    "secondary": "#7f9cf5",
    "accent": "#4fd1c5",
    "success": "#68d391",
    "warning": "#f6ad55",
    "error": "#fc8181",
    "foreground": "#e6edf3",
    "background": "#0d1117",
    "surface": "#161b22",
    "panel": "#1c2128",
    "text-muted": "#8b949e",
    # Status-bar-only palette. A midnight-indigo strip gives the telemetry a
    # distinct, polished layer without changing the rest of the terminal UI.
    "status-surface": "#12162b",
    "status-foreground": "#eef0ff",
    "status-muted": "#7f89ad",
    "status-ok": "#46e6a7",
    "status-model": "#86a5ff",
    "status-context": "#bd9cff",
    "status-time": "#5de4e7",
    "status-memory": "#f0a3ff",
    "status-warning": "#ffc266",
    "status-error": "#ff718d",
}

LIGHT_PALETTE = {
    "primary": "#0e7c7b",
    "secondary": "#4c63d2",
    "accent": "#0e7c7b",
    "success": "#2f855a",
    "warning": "#b7791f",
    "error": "#c53030",
    "foreground": "#1a202c",
    "background": "#ffffff",
    "surface": "#f0f2f5",
    "panel": "#e6e9ef",
    "text-muted": "#5a6472",
    # The light counterpart keeps the same hue mapping with deeper inks on a
    # cool lavender surface, preserving contrast in bright terminal profiles.
    "status-surface": "#ebeefe",
    "status-foreground": "#20264d",
    "status-muted": "#626b90",
    "status-ok": "#087a55",
    "status-model": "#4059c7",
    "status-context": "#6d47c9",
    "status-time": "#087582",
    "status-memory": "#a13da7",
    "status-warning": "#945800",
    "status-error": "#c33052",
}

_TRUE = {"1", "true", "yes", "on"}
_FALSE = {"0", "false", "no", "off"}
_LIGHT_TERM_PROGRAMS = {"Apple_Terminal"}


def detect_light_mode(env: Mapping[str, str] | None = None) -> bool:
    """Return whether terminal hints indicate a light background."""
    e = env if env is not None else os.environ

    theme_flag = str(e.get("ECHO_TUI_THEME", "")).strip().lower()
    if theme_flag == "light":
        return True
    if theme_flag == "dark":
        return False

    light_flag = str(e.get("ECHO_TUI_LIGHT", "")).strip().lower()
    if light_flag in _TRUE:
        return True
    if light_flag in _FALSE:
        return False

    colorfgbg = str(e.get("COLORFGBG", "")).strip()
    if colorfgbg:
        last = colorfgbg.split(";")[-1]
        if last.isdigit():
            background = int(last)
            if background in (7, 15):
                return True
            if 0 <= background < 16:
                return False

    return str(e.get("TERM_PROGRAM", "")).strip() in _LIGHT_TERM_PROGRAMS


def active_palette(env: Mapping[str, str] | None = None) -> dict[str, str]:
    """Return a copy of the palette appropriate for the current terminal."""
    source = LIGHT_PALETTE if detect_light_mode(env) else DARK_PALETTE
    return dict(source)


def ansi(role: str, env: Mapping[str, str] | None = None) -> str:
    """Return a 24-bit ANSI foreground escape for a named palette role."""
    value = active_palette(env).get(role)
    if value is None:
        raise KeyError(f"Unknown palette role: {role}")
    red, green, blue = (int(value[i:i + 2], 16) for i in (1, 3, 5))
    return f"\033[38;2;{red};{green};{blue}m"
