"""Progress heartbeat — level-triggered feedback for long-running turns.

Sits on top of the existing edge-triggered tool events. A per-turn timer
periodically reports "still working — elapsed — current activity" and keeps
the typing indicator alive, sealing once the final answer is delivered.
"""

from __future__ import annotations

import time
from dataclasses import dataclass, field


@dataclass
class ActivitySnapshot:
    elapsed_sec: float
    phase: str
    current_tool: str | None


@dataclass
class SharedActivityState:
    """Written by the inference stage, read by ProgressHeartbeat (one-way)."""

    started_at: float
    current_tool: str | None = None
    phase: str = "thinking"  # thinking | calling_tool | generating
    last_visible_feedback_at: float = field(default=0.0)

    def enter_tool(self, name: str) -> None:
        self.current_tool = name
        self.phase = "calling_tool"

    def exit_tool(self) -> None:
        self.current_tool = None
        self.phase = "thinking"

    def set_generating(self) -> None:
        self.phase = "generating"

    def mark_visible_feedback(self, now: float | None = None) -> None:
        self.last_visible_feedback_at = now if now is not None else time.monotonic()

    def since_last_feedback(self, now: float | None = None) -> float:
        ref = now if now is not None else time.monotonic()
        base = self.last_visible_feedback_at or self.started_at
        return ref - base

    def snapshot(self, now: float | None = None) -> ActivitySnapshot:
        ref = now if now is not None else time.monotonic()
        return ActivitySnapshot(
            elapsed_sec=ref - self.started_at,
            phase=self.phase,
            current_tool=self.current_tool,
        )
