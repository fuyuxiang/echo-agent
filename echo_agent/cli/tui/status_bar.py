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
    if clamped >= 80:
        color = "red"
    elif clamped >= 50:
        color = "yellow"
    else:
        color = "green"
    bar = "█" * filled + "░" * empty
    return f"[{color}]{bar}[/{color}]"


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

    def _compose_text(self) -> str:
        segments: list[str] = []

        # 0. Connection + session
        conn = "[green]●已连接[/green]" if self._ok else "[red]○已断开[/red]"
        if self._session:
            segments.append(f"{conn} {self._session}")
        else:
            segments.append(conn)

        # 1. Model
        model_display = self._model or "—"
        segments.append(f"[bold cyan]⚡ {model_display}[/bold cyan]")

        # 2. Context gauge
        if self._context_max > 0:
            used_str = _fmt_tokens(self._context_used)
            max_str = _fmt_tokens(self._context_max)
            percent = min(100, round(self._context_used / self._context_max * 100))
            bar = _ctx_bar(percent)
            segments.append(f"{used_str}/{max_str} {bar} {percent}%")
        else:
            segments.append("[dim]ctx —[/dim]")

        # 3. Timer
        if self._turn_start is not None:
            elapsed = time.time() - self._turn_start
            segments.append(f"[bold]⏱ {_fmt_duration(elapsed)}[/bold]")
        elif self._turn_elapsed > 0:
            segments.append(f"⏱ {_fmt_duration(self._turn_elapsed)}")
        else:
            segments.append("[dim]⏱ 0s[/dim]")

        # 4. Cost
        segments.append(f"${self._cost:.4f}")

        # 5. Memory count
        segments.append(f"[magenta]🧠 {self._memory_count}[/magenta]")

        return " │ ".join(segments)

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

    def start_turn_timer(self) -> None:
        self._turn_start = time.time()
        if self._timer is not None:
            self._timer.resume()
        self._refresh()

    def stop_turn_timer(self) -> None:
        if self._turn_start is not None:
            self._turn_elapsed = time.time() - self._turn_start
            self._turn_start = None
        if self._timer is not None:
            self._timer.pause()
        self._refresh()
