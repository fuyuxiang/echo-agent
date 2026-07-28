"""Glyph set for transcript decoration, selectable by ``ECHO_TUI_ICONS``.

The transcript used to hard-code emoji (🔧 🧠 💭 ⏳ ✍ 🧬 ⚠️ 💰) as its line
markers. Emoji are double-width in some terminals and single-width in others —
and the VS16 variation selector on ⚠️/✍ makes even one terminal inconsistent
between them — so every marked line started at a different column and the
transcript never aligned. They also all read at the same visual weight, which
is exactly wrong: a tool trace should recede behind the answer.

The default set is therefore narrow, single-column geometry: ``●`` for actors,
``✓``/``✗`` for outcomes, ``▸``/``▾`` for disclosure, ``├─ └─ │`` for tree
rails. Colour (not glyph) carries the semantics, which is what lets a trace line
be quiet. ``ECHO_TUI_ICONS=emoji`` restores the old pictographs for users who
prefer them; ``ascii`` drops to pure 7-bit for terminals without the box glyphs.

Pure module (env read once at import, overridable via ``resolve_glyphs``) so
widgets can be unit-tested against any set without a live screen.
"""

from __future__ import annotations

import os
from collections.abc import Mapping
from dataclasses import dataclass


@dataclass(frozen=True)
class GlyphSet:
    """Every non-textual mark the transcript draws.

    ``cognitive`` maps a ``cog_type`` to its line marker; the rest are shared
    structural marks. Frozen so a set can be safely module-level shared.
    """

    name: str
    reply: str            # agent's voice
    user: str             # the human turn's sigil
    tool: str             # a tool invocation
    ok: str               # succeeded
    fail: str             # failed
    pending: str          # still running
    unfinished: str       # never got its result (interrupt / error)
    collapsed: str        # closed disclosure
    expanded: str         # open disclosure
    rail: str             # tree vertical
    branch: str           # tree tee
    branch_last: str      # tree elbow
    sep: str              # inline separator between fields
    cognitive: Mapping[str, str]  # cog_type -> line marker


_NARROW_COG = {
    "memory_recalled": "◈",
    "memory_written": "◈",
    "thinking": "◇",
    "tool_call": "●",
    "approval_request": "!",
    "cost_update": "$",
    "heartbeat": "·",
    "evolution": "✦",
}

NARROW = GlyphSet(
    name="narrow",
    reply="●", user="❯", tool="●",
    ok="✓", fail="✗", pending="…", unfinished="–",
    collapsed="▸", expanded="▾",
    rail="│ ", branch="├─ ", branch_last="└─ ",
    sep="·",
    cognitive=_NARROW_COG,
)

EMOJI = GlyphSet(
    name="emoji",
    reply="●", user="❯", tool="🔧",
    ok="✓", fail="✗", pending="…", unfinished="–",
    collapsed="▸", expanded="▾",
    rail="│ ", branch="├─ ", branch_last="└─ ",
    sep="·",
    cognitive={
        "memory_recalled": "🧠", "memory_written": "✍", "thinking": "💭",
        "tool_call": "🔧", "approval_request": "⚠️", "cost_update": "💰",
        "heartbeat": "⏳", "evolution": "🧬",
    },
)

# Terminals without box-drawing/geometric coverage (some Windows consoles, plain
# `TERM=dumb` pipes). Everything is 7-bit, so alignment holds unconditionally.
ASCII = GlyphSet(
    name="ascii",
    reply="*", user=">", tool="-",
    ok="+", fail="x", pending="...", unfinished="-",
    collapsed=">", expanded="v",
    rail="|  ", branch="|- ", branch_last="`- ",
    sep="-",
    cognitive={k: "-" for k in _NARROW_COG},
)

_SETS = {"narrow": NARROW, "emoji": EMOJI, "ascii": ASCII}


def resolve_glyphs(env: Mapping[str, str] | None = None) -> GlyphSet:
    """Glyph set named by ``ECHO_TUI_ICONS``; NARROW for anything unrecognised."""
    source = os.environ if env is None else env
    return _SETS.get(str(source.get("ECHO_TUI_ICONS", "")).strip().lower(), NARROW)


# Resolved once at import: the set is a rendering constant for the life of the
# process (no live re-theming path exists for glyphs, unlike /theme for colours).
GLYPHS = resolve_glyphs()


def cog_glyph(cog_type: str, glyphs: GlyphSet | None = None) -> str:
    """Line marker for a cognitive frame; a neutral dot for unknown types."""
    return (glyphs or GLYPHS).cognitive.get(cog_type, "·")
