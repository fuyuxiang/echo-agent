"""Enhanced bottom status bar with model, context gauge, timer, cost, memory count.

Layout:  ⚡ model │ 12.8K/128K [██░░░░░░] 10% │ ⏱ 2.3s │ $0.0042 │ 🧠 47
"""

from __future__ import annotations

import time

from textual.widgets import Static


def _fmt_tokens(n: int) -> str:
    if n >= 1_000_000:
        return f"{n / 1_000_000:.1f}M"
    if n >= 1_000:
        return f"{n / 1_000:.1f}K"
    return str(n)


def _ctx_bar(percent: int, width: int = 10) -> str:
    clamped = max(0, min(100, percent))
    filled = round(clamped / 100 * width)
    empty = width - filled
    # Theme tokens (not raw ANSI) so the gauge adapts to light/dark — green/amber/
    # red were illegible on the light palette's white surface.
    if clamped >= 80:
        color = "$error"
    elif clamped >= 50:
        color = "$warning"
    else:
        color = "$success"
    bar = "█" * filled + "░" * empty
    return f"[{color}]{bar}[/]"


def _fmt_duration(seconds: float) -> str:
    s = max(0, int(seconds))
    if s < 60:
        return f"{s}s"
    m = s // 60
    remainder = s % 60
    if m < 60:
        return f"{m}m {remainder}s" if remainder else f"{m}m"
    h = m // 60
    return f"{h}h {m % 60}m"


class StatusBar(Static):
    def __init__(self) -> None:
        self._session = ""
        self._model = ""
        self._cost = 0.0
        self._ok = False
        self._context_used = 0
        self._context_max = 0
        self._memory_count = 0
        self._turn_start: float | None = None
        self._turn_elapsed: float = 0.0
        # Turn-active state is tracked separately from the display timer. A turn
        # spans many LLM calls (tool rounds, clarify waits, reflection reruns);
        # the elapsed-time display runs continuously across all of them and only
        # freezes when the turn ends. The Ctrl+C guard must key off this flag,
        # never the timer, so it keeps sending interrupts for the whole turn.
        self._turn_active: bool = False
        self._timer = None
        self._mounted = False
        super().__init__(self._compose_text())

    def on_mount(self) -> None:
        self._mounted = True
        self._timer = self.set_interval(1.0, self._tick, pause=True)
        self.update(self._compose_text())

    def _tick(self) -> None:
        if self._turn_start is not None:
            self.update(self._compose_text())
        else:
            if self._timer is not None:
                self._timer.pause()

    def _tier(self) -> str:
        """Responsive tier from the current width. Narrow terminals drop the
        heavier segments (context gauge, cost, memory) instead of letting the
        single-line bar overflow and clip mid-field.
          wide  (>=80): connection + model + context + timer + cost + memory
          mid   (>=50): connection + model + timer + cost
          narrow(< 50): connection + model + timer"""
        try:
            width = self.size.width or self.app.size.width
        except Exception:
            width = 0
        if width and width < 50:
            return "narrow"
        if width and width < 80:
            return "mid"
        return "wide"

    def _compose_text(self) -> str:
        tier = self._tier()
        segments: list[str] = []

        # 0. Connection + session (all tiers). Theme tokens so the light palette
        # stays legible — raw green/red on white failed the contrast bar.
        conn = "[$success]●已连接[/]" if self._ok else "[$error]○已断开[/]"
        if self._session and tier == "wide":
            segments.append(f"{conn} {self._session}")
        else:
            segments.append(conn)

        # 1. Model (all tiers)
        model_display = self._model or "—"
        segments.append(f"[b $accent]⚡ {model_display}[/]")

        # 2. Context gauge (wide only)
        if tier == "wide":
            if self._context_max > 0:
                used_str = _fmt_tokens(self._context_used)
                max_str = _fmt_tokens(self._context_max)
                percent = min(100, round(self._context_used / self._context_max * 100))
                bar = _ctx_bar(percent)
                segments.append(f"{used_str}/{max_str} {bar} {percent}%")
            else:
                segments.append("[$text-muted]上下文 —[/]")

        # 3. Timer (all tiers)
        if self._turn_start is not None:
            elapsed = time.time() - self._turn_start
            segments.append(f"[b]⏱ {_fmt_duration(elapsed)}[/b]")
        elif self._turn_elapsed > 0:
            segments.append(f"⏱ {_fmt_duration(self._turn_elapsed)}")
        else:
            segments.append("[$text-muted]⏱ 0s[/]")

        # 4. Cost (wide + mid)
        if tier in ("wide", "mid"):
            segments.append(f"${self._cost:.4f}")

        # 5. Memory count (wide only)
        if tier == "wide":
            segments.append(f"[$secondary]🧠 {self._memory_count}[/]")

        return " │ ".join(segments)

    def on_resize(self) -> None:
        # Re-render on width change so the responsive tier updates live.
        self._refresh()

    def _refresh(self) -> None:
        if not self._mounted:
            return
        self.update(self._compose_text())

    def set_session(self, key: str) -> None:
        self._session = key
        self._refresh()

    def set_model(self, name: str) -> None:
        self._model = name
        self._refresh()

    def set_cost(self, total: float) -> None:
        self._cost = total
        self._refresh()

    def set_connection(self, ok: bool) -> None:
        self._ok = ok
        self._refresh()

    def set_context(self, used: int, max_tokens: int) -> None:
        self._context_used = used
        self._context_max = max_tokens
        self._refresh()

    def set_memory_count(self, count: int) -> None:
        self._memory_count = count
        self._refresh()

    @property
    def is_turn_active(self) -> bool:
        """True from turn start until stop_turn_timer runs.

        Tracked separately from the elapsed-time display, which pauses on its
        own schedule. This is the status bar's own view of the turn; the Ctrl+C
        interrupt guard does NOT read it — app.py consults the turn tracker
        (``_turns.has_active_primary``), which also covers uncorrelated
        in-flight work this widget cannot see.
        """
        return self._turn_active

    def start_turn_timer(self) -> None:
        self._turn_active = True
        self._turn_start = time.time()
        if self._timer is not None:
            self._timer.resume()
        self._refresh()

    def pause_turn_timer(self) -> None:
        """Freeze the elapsed-time display at the current duration. Called by
        stop_turn_timer when the turn ends; NOT called per LLM round, so the
        timer runs continuously across a multi-round turn and shows the whole
        turn's duration."""
        if self._turn_start is not None:
            self._turn_elapsed = time.time() - self._turn_start
            self._turn_start = None
        if self._timer is not None:
            self._timer.pause()
        self._refresh()

    def stop_turn_timer(self) -> None:
        """End the turn: freeze the display AND clear the active flag. Called
        when the final reply arrives."""
        self._turn_active = False
        self.pause_turn_timer()
