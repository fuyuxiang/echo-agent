"""Live activity line — the single row that says what the agent is doing NOW.

Replaces the in-transcript heartbeat block. That block was mounted into the
scrollback keyed by ``inbound_event_id``, so its position was fixed at the
moment the first heartbeat arrived while tool lines kept appending *below* it:
the "still working" notice ended up above content that had already finished,
which reads backwards. It also carried a randomly rotating reassurance phrase
("马上就好" / "还在跑" / …) that conveyed nothing and re-rendered on every beat.

Docked directly above the input row, this line is unambiguously about the
present: it names the current phase (and the running tool when there is one),
counts in-flight tools, and shows the turn's own elapsed clock. It disappears
when no turn is running, so an idle screen ends at the last real content.

``render_text`` is a pure function of the widget's state plus an injected
``now``, so the whole display is unit-testable without a live screen — matching
the convention in blocks.py.
"""

from __future__ import annotations

import time

from rich.markup import escape
from textual.widgets import Static

from echo_agent.cli.tui.blocks import humanize_tool
from echo_agent.cli.tui.glyphs import GLYPHS

# Braille spinner: single-column in every terminal that renders it, so the line
# never shifts as the frame advances.
_FRAMES = ("⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏")
_ASCII_FRAMES = ("|", "/", "-", "\\")

_STAGE_LABEL = {
    "thinking": "思考中",
    "calling_tool": "调用工具",
    "generating": "正在组织答案",
}
_FALLBACK_STAGE = "处理中"


def _fmt_elapsed(seconds: float) -> str:
    """Sub-minute durations keep a decimal so short turns still visibly tick."""
    s = max(0.0, seconds)
    if s < 10:
        return f"{s:.1f}s"
    if s < 60:
        return f"{int(s)}s"
    m = int(s) // 60
    rem = int(s) % 60
    return f"{m}m {rem}s" if rem else f"{m}m"


class ActivityLine(Static):
    """One-row live status, docked above the prompt. Hidden while idle."""

    def __init__(self) -> None:
        self._active = False
        self._stage = ""
        self._tool = ""
        self._running_tools = 0
        self._started: float | None = None
        self._frame = 0
        self._timer = None
        self._mounted = False
        super().__init__("")

    def on_mount(self) -> None:
        self._mounted = True
        # One timer drives both the spinner and the clock. Paused while idle so
        # a quiet session costs no repaints.
        self._timer = self.set_interval(0.1, self._tick, pause=True)
        self._apply()

    def _tick(self) -> None:
        self._frame += 1
        self._apply()

    def _apply(self) -> None:
        # Nothing to paint before mount: `display` and `update` both reach into
        # the live style/render machinery, which does not exist yet (frames can
        # arrive during startup, and the pure-render unit tests construct the
        # widget without a screen). on_mount replays this once it is real.
        if not self._mounted:
            return
        # `display` (not remove/mount) so the row collapses to zero
        # height while idle without disturbing the layout above it.
        self.display = self._active
        self.update(self.render_text())

    # --- state transitions ---

    def start(self) -> None:
        """A turn began. Resets the phase so a new turn never inherits the
        previous one's label, and restarts the clock."""
        self._active = True
        self._stage = ""
        self._tool = ""
        self._running_tools = 0
        self._started = time.time()
        if self._timer is not None:
            self._timer.resume()
        self._apply()

    def stop(self) -> None:
        """The turn ended (reply landed, error, interrupt, disconnect). The row
        hides rather than freezing on a stale phase — a settled turn's history
        lives in the transcript, not here."""
        self._active = False
        self._stage = ""
        self._tool = ""
        self._running_tools = 0
        self._started = None
        if self._timer is not None:
            self._timer.pause()
        self._apply()

    def set_stage(self, stage: str) -> None:
        """Adopt a heartbeat's phase (``thinking``/``calling_tool``/
        ``generating``). Also revives the line: heartbeats can arrive for work
        the client never saw start (a turn accepted before a reconnect)."""
        self._stage = stage or ""
        if not self._active:
            self.start()
            self._stage = stage or ""
        self._apply()

    def tool_started(self, name: str) -> None:
        self._tool = name or ""
        self._running_tools += 1
        if not self._active:
            self.start()
            self._tool = name or ""
            self._running_tools = 1
        self._apply()

    def tool_finished(self) -> None:
        self._running_tools = max(0, self._running_tools - 1)
        if self._running_tools == 0:
            # The named tool is gone; fall back to the phase label rather than
            # keeping a finished tool's name on a line that means "now".
            self._tool = ""
        self._apply()

    # --- pure render ---

    @property
    def is_active(self) -> bool:
        return self._active

    def render_text(self, *, now: float | None = None) -> str:
        if not self._active:
            return ""
        frames = _ASCII_FRAMES if GLYPHS.name == "ascii" else _FRAMES
        spin = frames[self._frame % len(frames)]
        if self._tool:
            label = f"{_STAGE_LABEL.get('calling_tool', '')} {humanize_tool(self._tool)}".strip()
        else:
            label = _STAGE_LABEL.get(self._stage, _FALLBACK_STAGE)
        parts = [f"[$accent]{spin}[/] [b]{escape(label)}[/b]"]
        if self._started is not None:
            elapsed = (now if now is not None else time.time()) - self._started
            parts.append(f"[$text-muted]{_fmt_elapsed(elapsed)}[/]")
        if self._running_tools > 1:
            parts.append(f"[$text-muted]{self._running_tools} 个工具进行中[/]")
        body = f" [$text-muted]{GLYPHS.sep}[/] ".join(parts)
        return f"{body}  [$text-muted]Ctrl+C 中断[/]"
