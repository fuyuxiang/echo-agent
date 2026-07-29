"""Live activity line — the row that says what the agent is doing NOW, and how
the last turn ended.

Replaces the in-transcript heartbeat block. That block was mounted into the
scrollback keyed by ``inbound_event_id``, so its position was fixed at the
moment the first heartbeat arrived while tool lines kept appending *below* it:
the "still working" notice ended up above content that had already finished,
which reads backwards. It also carried a randomly rotating reassurance phrase
("马上就好" / "还在跑" / …) that conveyed nothing and re-rendered on every beat.

Docked directly above the input row, this line is unambiguously about the
present: it names the current phase (and the running tool when there is one),
counts in-flight tools, and shows the turn's own elapsed clock.

It then SETTLES rather than disappearing. Hiding the row on completion made the
only moving thing on screen vanish along with the elapsed clock, and the row
collapsed to zero height — so a finished turn and a hung one looked identical
("it was scrolling, then it just stopped"). Worse, all four end paths (reply,
gateway error, disconnect, reconnect) called the same ``stop()``, so success and
failure were visually equivalent. Now the row keeps one quiet terminal line
naming the outcome, its duration and what it did, and only clears when the next
turn starts (or ``reset()`` on /clear). Idle-from-boot still renders nothing, so
a fresh screen ends at the last real content.

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

# How a turn ended: label + theme colour. Distinct wording per outcome because
# these used to be one indistinguishable `stop()` — "完成" and a socket drop must
# not look the same. Keys are the `outcome` argument to settle().
_OUTCOME = {
    "done": ("完成", "$success"),
    "interrupted": ("已中断", "$warning"),
    "error": ("出错", "$error"),
    "disconnected": ("连接已断开", "$error"),
}
_FALLBACK_OUTCOME = ("已结束", "$text-muted")


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
    """One-row status docked above the prompt: live while a turn runs, then a
    settled summary of how it ended. Blank only before the first turn."""

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
        # --- settled state, shown after the turn ends ---
        # Outcome key from _OUTCOME, or "" when nothing has finished yet (fresh
        # boot). Kept separate from _active so a settled row survives until the
        # NEXT turn starts, which is the whole point: the user needs a visible
        # "this is over" that outlives the frame that ended it.
        self._outcome = ""
        self._final_elapsed = 0.0
        # Tools the finished turn actually ran, so the settled line can say
        # "完成 · 12.4s · 3 个工具" instead of a bare "完成". Tallied separately
        # from _tools, which holds only what is currently running and drains as
        # calls finish; _counted keeps the tally idempotent per call id.
        self._tools_seen = 0
        self._counted: set[str] = set()
        # A stop has been requested but the turn has not converged yet. The
        # interrupt is cooperative (polled at the loop's checkpoints), so this
        # window is user-visible; see note_stopping.
        self._stopping = False
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
        # `display` (not remove/mount) so the row collapses to zero height
        # before the first turn without disturbing the layout above it. A
        # settled row keeps its height: that line IS the completion signal.
        self.display = self._active or bool(self._outcome)
        self.update(self.render_text())

    # --- state transitions ---

    def start(self) -> None:
        """A turn began. Resets the phase so a new turn never inherits the
        previous one's label, restarts the clock, and drops the previous turn's
        settled summary — the row is about this turn from here on."""
        self._active = True
        self._stage = ""
        self._tools.clear()
        self._outcome = ""
        self._final_elapsed = 0.0
        self._tools_seen = 0
        self._counted.clear()
        self._stopping = False
        self._started = time.time()
        if self._timer is not None:
            self._timer.resume()
        self._apply()

    def note_stopping(self) -> None:
        """A stop was requested (Ctrl+C) but the turn has not converged yet.

        The interrupt is cooperative: the gateway only polls the flag at the
        inference loop's checkpoints, so seconds can pass before the turn
        actually ends. Ctrl+C previously touched this row not at all — it kept
        spinning "调用工具 …" as if nothing had been asked, and then settled as
        "完成", crediting a cancelled turn with success. This flips the label to
        "正在停止" immediately and makes the eventual settle report 已中断.
        """
        if not self._active:
            return
        self._stopping = True
        self._apply()

    def settle(self, outcome: str = "done") -> None:
        """The turn ended: freeze the row into a one-line terminal summary.

        Named for what it does, unlike the old ``stop()``, which hid the row
        entirely. That left the screen with no completion signal at all — the
        spinner and the clock both vanished and the row collapsed to zero height,
        so "finished" and "hung" were the same picture. Keeping a settled line
        (outcome + duration + tool count) is the answer to "it stopped moving,
        is it done or stuck?".

        ``outcome`` is a key of _OUTCOME: done / interrupted / error /
        disconnected. Unknown values degrade to a neutral "已结束" rather than
        raising on a decoration path.

        A no-op when there is nothing to settle (no active turn and no prior
        outcome), so a stray end-frame on a fresh screen cannot invent a
        summary for a turn that never ran. But a settle while ALREADY settled
        does update the outcome: the four end paths overlap (a gateway error is
        routinely followed by a disconnect), and the later, more specific
        reason should win rather than being dropped.
        """
        if not self._active and not self._outcome:
            return
        if self._started is not None:
            self._final_elapsed = max(0.0, time.time() - self._started)
        # A turn the user asked to stop reports 已中断 even though it ends via the
        # ordinary reply path: the gateway's cooperative interrupt converges at a
        # checkpoint and emits a normal final frame, so on_user_reply_final would
        # otherwise credit a cancelled turn as "完成". An explicitly signalled
        # outcome (error/disconnected) still wins — that is a harder fact than the
        # user's intent.
        if self._stopping and (not outcome or outcome == "done"):
            self._outcome = "interrupted"
        else:
            self._outcome = outcome or "done"
        self._stopping = False
        self._active = False
        self._stage = ""
        self._tools.clear()
        self._started = None
        if self._timer is not None:
            self._timer.pause()
        self._apply()

    # Back-compat alias: `stop` is the verb every call site in app.py used
    # before the settled row existed, and it reads correctly at those call sites
    # ("the turn stopped"). Kept so the four end paths stay one-liners, and so
    # any external caller keeps working — it now settles as "done" instead of
    # hiding the row.
    def stop(self) -> None:
        """Deprecated spelling of ``settle("done")``."""
        self.settle("done")

    def reset(self) -> None:
        """Blank the row completely, including any settled summary.

        For /clear: the transcript it was summarising is gone, so a line saying
        "完成 · 8.1s" would refer to work no longer on screen.
        """
        self._active = False
        self._stage = ""
        self._tools.clear()
        self._started = None
        self._outcome = ""
        self._final_elapsed = 0.0
        self._tools_seen = 0
        self._counted.clear()
        self._stopping = False
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
        # Count first-time keys only, so a duplicate start frame for the same
        # call does not inflate the settled line's "N 个工具".
        if key not in self._tools:
            self._count_tool(key)
        self._tools[key] = name or ""
        self._apply()

    def tool_finished(self, call_id: str = "") -> None:
        if call_id and call_id in self._tools:
            del self._tools[call_id]
        elif self._tools:
            # No id (or an unknown one): drop an arbitrary entry so the count
            # still drains. Insertion order makes this the oldest running call.
            self._tools.pop(next(iter(self._tools)))
        elif call_id:
            # A finish frame for a call we never saw start. Frames are not
            # guaranteed to arrive in pairs — a reconnect mid-round replays the
            # outcome without the start, and `/details 工具 隐藏` does not change
            # what reaches us. Count it anyway, or the settled line would
            # under-report the work that was actually done.
            self._count_tool(call_id)
        self._apply()

    def _count_tool(self, key: str) -> None:
        """Tally a call id towards the settled line's "N 个工具".

        Kept separate from ``_tools`` (which holds only what is CURRENTLY
        running, and drains as calls finish) because the tally must survive the
        drain to still be reportable at settle time.
        """
        if key not in self._counted:
            self._counted.add(key)
            self._tools_seen += 1

    # --- pure render ---

    @property
    def is_active(self) -> bool:
        return self._active

    @property
    def is_settled(self) -> bool:
        """True while showing a finished turn's summary. Distinct from
        ``not is_active``, which is also true on a fresh screen."""
        return not self._active and bool(self._outcome)

    @property
    def stop_requested(self) -> bool:
        """True between Ctrl+C and the turn actually converging.

        Callers use this to tell an ordinary final frame apart from the one that
        ends a cancelled turn: the gateway's interrupt is cooperative and emits a
        normal final, which is otherwise indistinguishable from the intermediate
        finals a healthy turn produces.
        """
        return self._stopping

    def render_text(self, *, now: float | None = None) -> str:
        if not self._active:
            return self._render_settled()
        frames = _ASCII_FRAMES if GLYPHS.name == "ascii" else _FRAMES
        spin = frames[self._frame % len(frames)]
        # A requested stop outranks whatever the turn was doing: the user's last
        # action was Ctrl+C, so that is what the row must acknowledge. The spinner
        # keeps turning because the turn genuinely is still winding down.
        if self._stopping:
            label = "正在停止"
        else:
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
        # No interrupt hint once a stop is already in flight — repeating it would
        # invite a second Ctrl+C, which the app reads as the exit guard.
        if self._stopping:
            return body
        return f"{body}  [$text-muted]Ctrl+C 中断[/]"

    def _render_settled(self) -> str:
        """The terminal line for a finished turn, or "" before the first one.

        Deliberately quiet: a static glyph instead of the spinner (nothing is
        moving), the outcome in its own colour, and the duration the turn took.
        No "Ctrl+C 中断" hint — there is nothing left to interrupt, and leaving it
        there was itself a reason the old row read as "still working".
        """
        if not self._outcome:
            return ""
        label, color = _OUTCOME.get(self._outcome, _FALLBACK_OUTCOME)
        mark = GLYPHS.ok if self._outcome == "done" else GLYPHS.unfinished
        parts = [f"[{color}]{mark}[/] [b {color}]{escape(label)}[/]"]
        # Duration is the point of the line for a long turn: it retroactively
        # explains the wait the user just sat through.
        if self._final_elapsed > 0:
            parts.append(f"[$text-muted]{_fmt_elapsed(self._final_elapsed)}[/]")
        if self._tools_seen > 0:
            parts.append(f"[$text-muted]{self._tools_seen} 个工具[/]")
        body = f" [$text-muted]{GLYPHS.sep}[/] ".join(parts)
        return f"{body}  [$text-muted]可以继续输入[/]"
