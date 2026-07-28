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
        # Running tools by call id. A bare count plus "last started name" showed a
        # tool that had already finished whenever parallel calls completed out of
        # order: start A, start B, finish B → count 1, but the line still said B.
        self._tools: dict[str, str] = {}
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
        self._tools.clear()
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
        self._tools.clear()
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

    def tool_started(self, name: str, call_id: str = "") -> None:
        """Record a tool as running. ``call_id`` pairs it with its finish frame;
        frames without one fall back to a synthetic key so the count still
        tracks, at the cost of not being individually removable."""
        was_active = self._active
        if not was_active:
            self.start()
        key = call_id or f"_anon{len(self._tools)}"
        self._tools[key] = name or ""
        self._apply()

    def tool_finished(self, call_id: str = "") -> None:
        if call_id and call_id in self._tools:
            del self._tools[call_id]
        elif self._tools:
            # No id (or an unknown one): drop an arbitrary entry so the count
            # still drains. Insertion order makes this the oldest running call.
            self._tools.pop(next(iter(self._tools)))
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
        # Named from what is actually still running, never from a remembered
        # "last started" that may already have finished.
        running = [n for n in self._tools.values() if n]
        if running:
            label = f"{_STAGE_LABEL.get('calling_tool', '')} {humanize_tool(running[0])}".strip()
        elif self._tools:
            label = _STAGE_LABEL.get("calling_tool", _FALLBACK_STAGE)
        else:
            label = _STAGE_LABEL.get(self._stage, _FALLBACK_STAGE)
        parts = [f"[$accent]{spin}[/] [b]{escape(label)}[/b]"]
        if self._started is not None:
            elapsed = (now if now is not None else time.time()) - self._started
            parts.append(f"[$text-muted]{_fmt_elapsed(elapsed)}[/]")
        if len(self._tools) > 1:
            parts.append(f"[$text-muted]{len(self._tools)} 个工具进行中[/]")
        body = f" [$text-muted]{GLYPHS.sep}[/] ".join(parts)
        return f"{body}  [$text-muted]Ctrl+C 中断[/]"
