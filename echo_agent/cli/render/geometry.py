"""Fixed indentation for the inline renderer's two-level output.

Three constants replace four interacting mechanisms the Textual transcript
needs (app.tcss padding, turn_layout.rail_prefix, blocks.child_rail, and a
focus-state border compensated by negative padding). The reason they can be
constants here is that nothing is ever re-rendered: a line's prefix is decided
when it is printed and the printed bytes never move again. That also removes
the "which trace line is last" question that forced turn_layout to give up on
├─/└─ between blocks while blocks.child_rail still used them inside a block —
the inconsistency this geometry retires.

    ⏺ 编辑 schema.py        HEAD_INDENT  + glyph, content at column 2
      ⎿ +12 行 -3 行        CHILD_INDENT + hook,  content at column 4
        移除了 3 处分支      CONT_INDENT,         content at column 4

Scope constraint — ``ECHO_TUI_ICONS=claude`` belongs to this renderer only, and
is not a legal value for the full-screen TUI. The CLAUDE glyph set sets ``rail``
and ``branch`` to empty strings, which is right for a renderer that draws no
tree, but it breaks a geometry invariant the full-screen path depends on:
blocks.child_rail aligns a wrapped detail line with ``cont.ljust(len(elbow))``,
i.e. it assumes the elbow and the rail have the same width. With CLAUDE that
padding target collapses to 0 for a non-last row and 2 for a last row instead of
the uniform 3 every other set yields, so continuation lines land in the wrong
column (measured: 4 failures in tests/cli/tui/ under that env var, 0 on the
baseline sets). The inline renderer is immune because its indentation comes from
the constants below and never routes through ``turn_layout.rail_prefix`` or
``blocks.child_rail``; only ``branch_last`` is read, and only for its width.
"""

from __future__ import annotations

from echo_agent.cli.tui.glyphs import CLAUDE

# Main line: the glyph itself starts the row.
HEAD_INDENT = ""

# A reported result sits one level in from the action that produced it.
CHILD_INDENT = "  "

# A wrapped child line aligns with the child's content, without repeating the
# hook. Derived from CHILD_INDENT + the hook's width so the three stay in sync
# if the hook glyph is ever changed.
CONT_INDENT = " " * (len(CHILD_INDENT) + len(CLAUDE.branch_last))

# Wrapped answer lines align under the reply text, clearing the "⏺ " sigil.
REPLY_BODY_INDENT = " " * (len(CLAUDE.reply) + 1)


def head_prefix() -> str:
    return HEAD_INDENT


def child_prefix() -> str:
    return CHILD_INDENT


def cont_prefix() -> str:
    return CONT_INDENT


def reply_body_indent() -> str:
    return REPLY_BODY_INDENT
