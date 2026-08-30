"""Line-oriented output for the inline renderer.

Deliberately append-only: apart from the single spinner row, there is no
cursor-up / clear-screen API here at all. cli/channels/cli.py documents that
stdout cannot unprint, and config.schema's stream_optimistic_channels states
that only channels which can redraw in place may stream optimistically. So the
inline renderer prints process lines as they arrive and prints the answer once,
at the end, complete — nothing ever needs to move after it is written.

The spinner is the one exception, and it follows the pattern cli/ui.py already
established: animate only on a tty, and clear with "\\r\\x1b[2K" (carriage
return plus erase-line) so no stale characters survive at the end of the row.
On a pipe it degrades to a single static line, because \\r in captured output
is noise.
"""

from __future__ import annotations

import sys

from echo_agent.cli.render import ansi as A
from echo_agent.cli.render.geometry import (
    child_prefix, cont_prefix, head_prefix,
)
from echo_agent.cli.render.redact import mask_sensitive_strings
from echo_agent.cli.render.text import clip
from echo_agent.cli.render.tool import (
    fmt_duration_ms, humanize_tool, pick_object, summarize_result,
)
from echo_agent.cli.tui.glyphs import CLAUDE, GlyphSet

_SPINNER_FRAMES = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"
_CLEAR_LINE = "\r\033[2K"


class InlinePrinter:
    """Writes the transcript as plain appended lines.

    Every content method clears a live spinner first, so callers never have to
    remember the ordering — getting that wrong prints content on top of the
    spinner row.
    """

    def __init__(self, stream=None, glyphs: GlyphSet = CLAUDE) -> None:
        self._out = stream if stream is not None else sys.stdout
        self._g = glyphs
        # Whether the last thing written was a blank line (or nothing at all).
        # Used to collapse repeated blank() requests: the gap rule is "one blank
        # line where the kind of content changes", and callers may ask twice.
        self._at_blank = True
        self._wrote_anything = False
        self._spinner_live = False
        self._spinner_frame = 0

    @property
    def is_tty(self) -> bool:
        try:
            return bool(self._out.isatty())
        except (AttributeError, ValueError):
            return False

    def _write(self, text: str) -> None:
        try:
            self._out.write(text)
            self._out.flush()
        except (OSError, ValueError):
            # A closed or broken stream must not take the session down; the
            # user has already lost the display either way.
            pass

    def _line(self, text: str) -> None:
        self.spinner_clear()
        self._write(text + "\n")
        self._at_blank = False
        self._wrote_anything = True

    def blank(self) -> None:
        """Open one blank line, collapsing repeats and suppressing a leading one."""
        if self._at_blank or not self._wrote_anything:
            return
        self.spinner_clear()
        self._write("\n")
        self._at_blank = True

    def plain(self, text: str) -> None:
        self._line(text)

    def head(self, text: str, *, style: str = "") -> None:
        glyph = A.paint(self._g.reply, style) if style else self._g.reply
        self._line(f"{head_prefix()}{glyph} {text}")

    def child(self, text: str, *, dim: bool = True) -> None:
        body = A.paint(text, A.fg("text-muted")) if dim else text
        hook = A.paint(self._g.branch_last, A.fg("text-muted"))
        self._line(f"{child_prefix()}{hook}{body}")

    def cont(self, text: str) -> None:
        self._line(f"{cont_prefix()}{A.paint(text, A.fg('text-muted'))}")

    def tool_line(
        self,
        name: str,
        params: dict,
        status: str = "running",
        result_meta: dict | None = None,
        result_text: str = "",
        duration_ms: int | None = None,
    ) -> None:
        """One tool invocation: the action on the head line, its outcome below.

        Unlike the Textual block this does NOT flip in place from running to
        done — the running line stays where it was printed and the done frame
        prints the result as a child line under it.
        """
        verb = humanize_tool(name)
        obj = mask_sensitive_strings(pick_object(name, params))
        head = f"{verb} {obj}".rstrip() if obj else verb
        self.head(head, style=A.fg("accent"))
        if status == "running":
            self.child(self._g.pending)
            return
        if status == "interrupted":
            self.child(f"未完成 {self._g.unfinished}", dim=True)
            return
        ok = status == "ok"
        summary = mask_sensitive_strings(
            summarize_result(name, result_meta, result_text, ok)
        )
        took = fmt_duration_ms(duration_ms)
        if took:
            summary = f"{summary} {self._g.sep} {took}"
        mark = self._g.ok if ok else self._g.fail
        colour = A.fg("success") if ok else A.fg("error")
        self._line(
            f"{child_prefix()}"
            f"{A.paint(self._g.branch_last, A.fg('text-muted'))}"
            f"{A.paint(clip(summary, 200), A.fg('text-muted') if ok else A.fg('error'))} "
            f"{A.paint(mark, colour)}"
        )

    # --- spinner -------------------------------------------------------

    def spinner_start(self, label: str) -> None:
        if not self.is_tty:
            # Static one-liner: the run is not interactive, so an animated row
            # would only inject control characters into captured output.
            self._line(label)
            return
        self._spinner_live = True
        self._spinner_frame = 0
        self._paint_spinner(label)

    def spinner_update(self, label: str) -> None:
        if not self.is_tty:
            return
        self._spinner_frame += 1
        self._paint_spinner(label)

    def _paint_spinner(self, label: str) -> None:
        frame = _SPINNER_FRAMES[self._spinner_frame % len(_SPINNER_FRAMES)]
        self._spinner_live = True
        self._write(
            f"{_CLEAR_LINE}{A.paint(frame, A.fg('accent'))} "
            f"{A.paint(label, A.fg('text-muted'))}"
        )

    def spinner_clear(self) -> None:
        """Erase a live spinner row. No-op when none is live or not a tty."""
        if not self._spinner_live:
            return
        self._spinner_live = False
        self._write(_CLEAR_LINE)
