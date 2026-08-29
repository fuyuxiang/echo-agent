"""Text helpers shared by the Textual transcript and the inline renderer.

Moved verbatim out of cli/tui/blocks.py so the inline renderer can reuse them
without importing Textual. blocks.py re-exports all three names, so existing
importers keep working.
"""

from __future__ import annotations

# Glyphs older gateways prefixed onto their cognitive summary text. The client
# now owns the line marker (glyphs.py), so a summary arriving with one of these
# would render two markers side by side. Stripped on read rather than trusted,
# because the gateway and the cli are versioned independently.
_LEGACY_SUMMARY_GLYPHS = ("🧠", "✍", "💭", "🔧", "⚠️", "⚠", "💰", "⏳", "🧬", "•")


def strip_legacy_glyph(summary: str) -> str:
    text = str(summary).lstrip()
    for glyph in _LEGACY_SUMMARY_GLYPHS:
        if text.startswith(glyph):
            return text[len(glyph):].lstrip()
    return text


def clip(s: str, n: int) -> str:
    s = " ".join(str(s).split())
    return s if len(s) <= n else s[: n - 1] + "…"
