"""Session-level rate limiter — token bucket per session key."""

from __future__ import annotations

import time
from collections import OrderedDict


class SessionRateLimiter:
    """Token-bucket rate limiter keyed by session.

    Prevents a single user/session from flooding the system while
    allowing fair access across all sessions.
    """

    def __init__(self, rpm: int = 20, burst: int = 5, max_sessions: int = 10000):
        self._rpm = rpm
        self._burst = float(burst)
        self._refill_rate = rpm / 60.0
        self._buckets: OrderedDict[str, tuple[float, float]] = OrderedDict()
        self._max_sessions = max_sessions

    @property
    def enabled(self) -> bool:
        return self._rpm > 0

    def try_acquire(self, session_key: str) -> bool:
        if not self.enabled:
            return True

        now = time.monotonic()
        if session_key in self._buckets:
            tokens, last = self._buckets[session_key]
            elapsed = now - last
            tokens = min(self._burst, tokens + elapsed * self._refill_rate)
        else:
            tokens = self._burst
            last = now

        if tokens >= 1.0:
            self._buckets[session_key] = (tokens - 1.0, now)
            self._buckets.move_to_end(session_key)
            self._evict()
            return True

        self._buckets[session_key] = (tokens, now)
        # Keep rejected (actively flooding) sessions at the MRU end too —
        # otherwise eviction pressure could drop their bucket and hand them
        # a fresh full burst.
        self._buckets.move_to_end(session_key)
        self._evict()
        return False

    def _evict(self) -> None:
        while len(self._buckets) > self._max_sessions:
            self._buckets.popitem(last=False)
