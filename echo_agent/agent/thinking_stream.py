"""Pacing for streamed model reasoning.

Reasoning arrives from the provider one token at a time. Publishing a cognitive
frame per token would put thousands of frames on the bus for a single round and
make the TUI repaint faster than it can be read, so deltas are accumulated here
and released on a char/time budget — the same idea as TokenStreamPublisher, kept
separate because a thinking trace is a *replaceable snapshot* (each frame
carries the whole trace so far and supersedes the previous one) rather than an
append-only stream of chunks.

Pure logic: no bus, no clock of its own beyond an injectable ``clock``, so the
pacing rules are unit-testable without a running loop.
"""

from __future__ import annotations

import time
import uuid
from collections.abc import Callable

# Mirrors the cap the post-hoc thinking event has always applied, so the
# streamed trace and the final one are trimmed at the same place and the block
# does not visibly rewrite itself when the round settles.
MAX_CHARS = 2000


class ThinkingStream:
    """Accumulates reasoning deltas and decides when to publish a snapshot."""

    def __init__(
        self,
        *,
        flush_chars: int = 80,
        flush_interval: float = 0.6,
        clock: Callable[[], float] = time.monotonic,
    ) -> None:
        self.thinking_id = "th_" + uuid.uuid4().hex[:12]
        self._flush_chars = max(1, flush_chars)
        self._flush_interval = max(0.05, flush_interval)
        self._clock = clock
        self._text = ""
        self._published_len = 0
        self._published = 0
        self._last_flush = 0.0

    @property
    def text(self) -> str:
        """The trace accumulated so far, already capped."""
        return self._text

    @property
    def streamed(self) -> bool:
        """Whether at least one snapshot was released.

        The caller uses this to decide what the end of the round means: a stream
        that produced frames needs its block either finalized or retracted,
        while one that never published behaves exactly as before this existed.
        """
        return self._published > 0

    def add(self, delta: str) -> str | None:
        """Absorb a delta; return the snapshot to publish, or None to hold.

        Returning the *whole* trace rather than the pending tail keeps the
        consumer stateless: a frame lost to a reconnect or dropped by the dedup
        window costs nothing, because the next one is self-contained.
        """
        if not delta:
            return None
        room = MAX_CHARS - len(self._text)
        if room <= 0:
            # Past the cap the visible text can no longer change, so further
            # frames would repaint identical content. The docked activity line
            # keeps signalling that the model is still thinking.
            return None
        self._text += delta[:room]
        now = self._clock()
        due = (
            # First delta goes out immediately: the point of streaming is that
            # something appears while the model is still working, and waiting a
            # full interval for the opening frame defeats that.
            self._published == 0
            or len(self._text) - self._published_len >= self._flush_chars
            or now - self._last_flush >= self._flush_interval
        )
        if not due:
            return None
        self._published += 1
        self._published_len = len(self._text)
        self._last_flush = now
        return self._text
