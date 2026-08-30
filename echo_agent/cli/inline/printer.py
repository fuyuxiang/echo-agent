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
from shutil import get_terminal_size

from rich.console import Console
from rich.markdown import Markdown
from rich.table import Table

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

    def set_stream(self, stream) -> None:
        """Switch to the stream active for the interactive prompt.

        ``prompt_toolkit.patch_stdout`` installs its proxy only while the input
        loop is running.  Delaying this binding keeps asynchronous gateway
        output above the editable prompt instead of painting through it.
        """
        self.spinner_clear()
        self._out = stream

    def note_external_line(self) -> None:
        """Tell the spacing model that the prompt already wrote a user line."""
        self.spinner_clear()
        self._at_blank = False
        self._wrote_anything = True

    def _line(self, text: str) -> None:
        self.spinner_clear()
        self._write(text + "\n")
        self._at_blank = False
        self._wrote_anything = True

    def blank(self) -> None:
        """Open one blank line, collapsing repeats and suppressing a leading one."""
        # A spinner is a transient row, not transcript content.  It still has
        # to be erased when blank() is the first permanent output operation.
        self.spinner_clear()
        if self._at_blank or not self._wrote_anything:
            return
        self._write("\n")
        self._at_blank = True

    def plain(self, text: str) -> None:
        self._line(text)

    def head(self, text: str, *, style: str = "", glyph: str = "") -> None:
        """A main line: the glyph that acts, then what it did.

        ``glyph`` overrides the set's default marker so a cognitive frame can
        print its own (✻ for thinking, ✦ for an approval request). It stays a
        parameter here rather than letting callers assemble the row with
        ``plain()``: the column a main line starts at is geometry's to decide
        (render/geometry.py exists so prefixes come from constants, not from
        each call site), and ``plain()`` means "print this verbatim", which
        would make main lines indistinguishable from body text to anything
        downstream that sorts output by line kind.

        rstrip'ed because an empty ``text`` would otherwise leave the marker
        with a trailing space — invisible on screen, but it shows up in
        captured output and in exports.
        """
        mark = glyph or self._g.reply
        painted = A.paint(mark, style) if style else mark
        self._line(f"{head_prefix()}{painted} {text}".rstrip())

    def child(self, text: str, *, dim: bool = True) -> None:
        body = A.paint(text, A.fg("text-muted")) if dim else text
        hook = A.paint(self._g.branch_last, A.fg("text-muted"))
        self._line(f"{child_prefix()}{hook}{body}")

    def cont(self, text: str) -> None:
        self._line(f"{cont_prefix()}{A.paint(text, A.fg('text-muted'))}")

    def reply(self, text: str) -> None:
        """Render one authoritative assistant reply, with Markdown support.

        A two-column Rich grid gives wrapped paragraphs the same fixed hanging
        indent as Claude Code: the actor glyph appears once and all reply text
        starts in the next column.  Non-TTY output is deliberately colourless
        and contains no cursor-control sequences.
        """
        body = str(text or "").strip()
        if not body:
            return
        self.spinner_clear()
        width = max(24, get_terminal_size((100, 24)).columns)
        colour = self.is_tty and A.supports_color()
        console = Console(
            file=self._out,
            force_terminal=colour,
            no_color=not colour,
            color_system="truecolor" if colour else None,
            width=width,
            soft_wrap=False,
            highlight=False,
        )
        table = Table.grid(padding=(0, 1), expand=False)
        table.add_column(no_wrap=True)
        table.add_column()
        table.add_row(
            A.paint(self._g.reply, A.fg("accent")),
            Markdown(body),
        )
        console.print(table)
        self._at_blank = False
        self._wrote_anything = True

    def notice(self, text: str, *, glyph: str = "·") -> None:
        self.head(text, glyph=glyph, style=A.fg("text-muted"))

    def error(self, text: str) -> None:
        self.head(text, glyph=self._g.fail, style=A.fg("error"))

    def clear_screen(self) -> None:
        """Clear only for an explicit /clear; never rewrite normal history."""
        self.spinner_clear()
        if self.is_tty:
            self._write("\033[2J\033[H")
        else:
            self._write("\n")
        self._at_blank = True
        self._wrote_anything = False

    def tool_line(
        self,
        name: str,
        params: dict,
        status: str = "running",
        result_meta: dict | None = None,
        result_text: str = "",
        duration_ms: int | None = None,
    ) -> None:
        """Print a complete tool block when no earlier start line is available.

        Live calls should use :meth:`tool_start` on the running frame and
        :meth:`tool_result` on the terminal frame. This convenience method is
        retained for terminal-only frames (for example after reconnect) and for
        callers that already have the whole invocation.
        """
        self.tool_start(name, params)
        if status == "running":
            self.child(self._g.pending)
            return
        self.tool_result(
            name, params, status, result_meta, result_text, duration_ms,
        )

    def tool_start(self, name: str, params: dict) -> None:
        """Append the stable action line as soon as execution begins.

        There is intentionally no permanent ``…`` child: the transient spinner
        already says the call is live, and leaving a pending marker in native
        scrollback after a successful completion would make settled work look
        unfinished forever.
        """
        verb = humanize_tool(name)
        obj = mask_sensitive_strings(pick_object(name, params))
        head = f"{verb} {obj}".rstrip() if obj else verb
        self.head(head, style=A.fg("accent"))

    def tool_result(
        self,
        name: str,
        params: dict,
        status: str,
        result_meta: dict | None = None,
        result_text: str = "",
        duration_ms: int | None = None,
        *,
        include_identity: bool = False,
    ) -> None:
        """Append the result beneath a previously printed action line.

        Parallel starts may interleave before their results. In that case
        ``include_identity`` repeats a short operand in the result so the user
        can correlate it without call ids or tree-drawing cursor tricks.
        """
        identity = ""
        if include_identity:
            verb = humanize_tool(name)
            obj = mask_sensitive_strings(pick_object(name, params))
            identity = f"{verb} {obj}".rstrip() if obj else verb
        if status == "interrupted":
            summary = "未完成"
            if identity:
                summary = f"{identity} · {summary}"
            self.child(f"{summary} {self._g.unfinished}", dim=True)
            return
        ok = status == "ok"
        summary = mask_sensitive_strings(
            summarize_result(name, result_meta, result_text, ok)
        )
        if identity:
            summary = f"{identity} · {summary}"
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
