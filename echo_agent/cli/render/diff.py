"""Unified-diff colouring, shared by the Textual transcript and the inline
renderer.

The palette is injected rather than hard-coded: Textual wants markup spans
([$success]…[/]) while a terminal wants raw ANSI. Both callers otherwise need
byte-identical line classification, so the classification lives here once and
only the wrapping differs.
"""

from __future__ import annotations

from dataclasses import dataclass

from rich.markup import escape

# Tools whose result text is a unified-style diff worth coloring line by line.
_DIFF_TOOLS = {"edit_file", "patch", "write_file"}


@dataclass(frozen=True)
class DiffStyle:
    """How to wrap one classified diff line.

    ``escape_content`` is part of the style, not a separate argument: Rich
    markup must have brackets in the diff body neutralised or diff content can
    inject tags, while a terminal renders brackets literally and escaping them
    would show stray backslashes.
    """

    added: str
    removed: str
    meta: str
    reset: str
    escape_content: bool


TEXTUAL_DIFF_STYLE = DiffStyle(
    added="[$success]", removed="[$error]", meta="[$text-muted]",
    reset="[/]", escape_content=True,
)

# 24-bit ANSI. Hues match the dark palette's success/error/text-muted so the
# inline renderer reads as the same product as the TUI.
ANSI_DIFF_STYLE = DiffStyle(
    added="\033[38;2;104;211;145m",
    removed="\033[38;2;252;129;129m",
    meta="\033[38;2;139;148;158m",
    reset="\033[0m",
    escape_content=False,
)


def colorize_diff(
    text: str, max_lines: int = 40, style: DiffStyle | None = None
) -> str:
    """Color a unified-diff-ish blob: +added lines green, -removed lines red,
    @@ hunk headers muted. Returns the styled blob, capped so a huge diff can't
    flood the output.

    ``style`` defaults to Textual markup for backward compatibility: existing
    callers (and tests) invoke this with one argument.
    """
    st = style or TEXTUAL_DIFF_STYLE
    out: list[str] = []
    lines = text.splitlines()
    for raw in lines[:max_lines]:
        line = escape(raw) if st.escape_content else raw
        if raw.startswith("+") and not raw.startswith("+++"):
            out.append(f"{st.added}{line}{st.reset}")
        elif raw.startswith("-") and not raw.startswith("---"):
            out.append(f"{st.removed}{line}{st.reset}")
        elif raw.startswith("@@") or raw.startswith("+++") or raw.startswith("---"):
            out.append(f"{st.meta}{line}{st.reset}")
        else:
            out.append(line)
    if len(lines) > max_lines:
        out.append(f"{st.meta}… (还有 {len(lines) - max_lines} 行){st.reset}")
    return "\n".join(out)
