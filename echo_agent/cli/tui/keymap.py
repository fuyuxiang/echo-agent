"""Pure key-decision state machine for PromptInput. No Textual imports — the
widget passes a snapshot (KeyContext) and applies the returned action. Priority:
panel open > history browse > plain edit."""

from __future__ import annotations

from dataclasses import dataclass


@dataclass
class KeyContext:
    key: str
    text: str
    cursor_row: int
    last_row: int
    panel_open: bool
    hist_idx: int
    hist_len: int


def decide_key(ctx: KeyContext) -> str:
    if ctx.panel_open:
        if ctx.key == "up":
            return "panel_prev"
        if ctx.key == "down":
            return "panel_next"
        if ctx.key in ("enter", "tab"):
            return "panel_accept"
        if ctx.key == "escape":
            return "panel_close"
        return "passthrough"
    if ctx.key == "enter":
        return "submit"
    if ctx.key == "shift+enter":
        return "newline"
    if ctx.key == "up" and ctx.cursor_row == 0 and ctx.hist_len > 0:
        return "history_prev"
    if ctx.key == "down" and ctx.cursor_row == ctx.last_row and ctx.hist_len > 0:
        return "history_next"
    return "passthrough"


def history_prev(idx: int, length: int) -> int:
    return max(0, idx - 1)


def history_next(idx: int, length: int) -> int:
    return min(length, idx + 1)
