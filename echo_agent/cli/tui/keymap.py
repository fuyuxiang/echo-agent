"""Pure key-decision state machine for PromptInput. No Textual imports — the
widget passes a snapshot (KeyContext) and applies the returned action. This is
the single authority for key priority: panel (while visible) > history browse >
plain edit. The widget only executes the side effect for whatever action it
returns; it does not re-decide priority itself."""

from __future__ import annotations

from dataclasses import dataclass


@dataclass
class KeyContext:
    key: str
    text: str
    cursor_row: int
    last_row: int
    panel_visible: bool          # completion panel is on screen
    panel_active: bool           # user has stepped into it with Up/Down
    hist_idx: int
    hist_len: int


def decide_key(ctx: KeyContext) -> str:
    # Panel keys take priority whenever the panel is *visible*, so Up/Down drive
    # the highlight (and Escape closes it) instead of the editor/history. The one
    # exception is Enter/Tab: those complete only once a selection is active;
    # otherwise they fall through so Enter still submits (the Critical fix) and
    # Tab reaches the editor. This keeps the whole priority ladder in one place.
    if ctx.panel_visible:
        if ctx.key == "up":
            return "panel_prev"
        if ctx.key == "down":
            return "panel_next"
        if ctx.key == "escape":
            return "panel_close"
        if ctx.key in ("enter", "tab") and ctx.panel_active:
            return "panel_accept"
    if ctx.key == "enter":
        return "submit"
    if ctx.key == "shift+enter":
        return "newline"
    if ctx.key == "up" and ctx.cursor_row == 0 and ctx.hist_len > 0:
        return "history_prev"
    if (
        ctx.key == "down"
        and ctx.cursor_row == ctx.last_row
        and ctx.hist_len > 0
        and ctx.hist_idx < ctx.hist_len
    ):
        return "history_next"
    return "passthrough"


def history_prev(idx: int, length: int) -> int:
    return max(0, idx - 1)


def history_next(idx: int, length: int) -> int:
    return min(length, idx + 1)
