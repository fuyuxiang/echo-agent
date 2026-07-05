"""Restart circuit breaker — suppress watchdog self-exits during a respawn storm.

The loop watchdog self-exits so a supervisor (launchd KeepAlive / systemd
Restart=always) respawns a clean process. But if the freeze recurs immediately
on every restart, that becomes a tight kill/respawn loop. This guard records
each self-exit to a small JSON file with a rolling time window; once the count
in the window exceeds ``max_restarts``, ``is_tripped()`` returns True and the
watchdog refuses to arm — pulling a human into the loop instead of thrashing.

Uses wall-clock time.time() intentionally: the window must survive across
process restarts, so a monotonic clock (which resets each process) is unusable.
"""

from __future__ import annotations

import json
import time
from pathlib import Path


class RestartGuard:
    def __init__(
        self,
        state_path: Path,
        *,
        max_restarts: int = 5,
        window_seconds: float = 3600.0,
        now: "callable" = time.time,
    ) -> None:
        self._path = state_path
        self._max = max_restarts
        self._window = window_seconds
        self._now = now

    def _load(self) -> list[float]:
        try:
            data = json.loads(self._path.read_text(encoding="utf-8"))
            stamps = data.get("restarts", []) if isinstance(data, dict) else []
            return [float(s) for s in stamps if isinstance(s, (int, float))]
        except (OSError, ValueError, TypeError):
            return []

    def _recent(self, stamps: list[float]) -> list[float]:
        cutoff = self._now() - self._window
        return [s for s in stamps if s >= cutoff]

    def recent_count(self) -> int:
        return len(self._recent(self._load()))

    def is_tripped(self) -> bool:
        return self.recent_count() >= self._max

    def record_restart(self) -> None:
        """Append a self-exit timestamp, pruning entries outside the window.

        Best-effort and never raises: this runs on the kill path just before
        os._exit, so a write failure must not prevent the exit.
        """
        try:
            stamps = self._recent(self._load())
            stamps.append(self._now())
            self._path.parent.mkdir(parents=True, exist_ok=True)
            self._path.write_text(
                json.dumps({"restarts": stamps}, ensure_ascii=False),
                encoding="utf-8",
            )
        except OSError:
            pass
