"""Echo TUI themes + light/dark auto-detection.

Two palettes (dark default, light for bright terminals) plus a detector.
Detection order (first decisive signal wins):

  1. ECHO_TUI_THEME = light|dark   — explicit override
  2. ECHO_TUI_LIGHT = 1/true/…     — explicit boolean
  3. COLORFGBG last field 7 or 15  — XFCE/rxvt/Terminal.app light profiles;
     any other 0..15 slot is treated as authoritatively dark
  4. TERM_PROGRAM light allow-list — Apple_Terminal defaults to a light profile

Anything undecided stays dark — the default Echo palette is the dark one.
"""

from __future__ import annotations

import os
from collections.abc import Mapping

from textual.theme import Theme

# Muted foreground for secondary text (tool operands, hints) so the eye lands on
# the primary content first. Shared shape across both palettes.
ECHO_THEME = Theme(
    name="echo",
    primary="#4fd1c5",     # teal — user/accent bar, headings
    secondary="#7f9cf5",   # indigo — cognitive (thinking/memory)
    accent="#4fd1c5",
    success="#68d391",     # green — tool ✓, connected
    warning="#f6ad55",     # amber — approvals, mid gauge
    error="#fc8181",       # soft red — tool ✗, disconnected
    foreground="#e6edf3",
    background="#0d1117",
    surface="#161b22",
    panel="#1c2128",
    dark=True,
    variables={"text-muted": "#8b949e"},
)

# Light palette: darker teals/indigos that stay legible on white. Same variable
# shape as the dark one so app.tcss ($primary/$boost/$primary-muted/…) resolves
# identically — only the hues change.
ECHO_THEME_LIGHT = Theme(
    name="echo-light",
    primary="#0e7c7b",     # deep teal — readable on white
    secondary="#4c63d2",   # indigo
    accent="#0e7c7b",
    success="#2f855a",     # green
    warning="#b7791f",     # amber
    error="#c53030",       # red
    foreground="#1a202c",
    background="#ffffff",
    surface="#f0f2f5",
    panel="#e6e9ef",
    dark=False,
    variables={"text-muted": "#5a6472"},
)

_TRUE = {"1", "true", "yes", "on"}
_FALSE = {"0", "false", "no", "off"}
_LIGHT_TERM_PROGRAMS = {"Apple_Terminal"}


def detect_light_mode(env: Mapping[str, str] | None = None) -> bool:
    """True if the terminal looks light. See module docstring for the order."""
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
            bg = int(last)
            if bg in (7, 15):
                return True
            if 0 <= bg < 16:
                # Any other 0..15 slot is the dark half — trust it as
                # authoritative so the TERM_PROGRAM allow-list can't override it.
                return False

    return str(e.get("TERM_PROGRAM", "")).strip() in _LIGHT_TERM_PROGRAMS


def resolve_theme_name(env: Mapping[str, str] | None = None) -> str:
    """Theme name to activate: 'echo-light' on a light terminal, else 'echo'."""
    return "echo-light" if detect_light_mode(env) else "echo"
