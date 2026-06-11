"""Tests for the session-level rate limiter."""

import time


from echo_agent.bus.rate_limiter import SessionRateLimiter


class TestSessionRateLimiter:
    def test_disabled_when_rpm_zero(self):
        limiter = SessionRateLimiter(rpm=0)
        assert not limiter.enabled
        assert limiter.try_acquire("any_session")

    def test_allows_burst(self):
        limiter = SessionRateLimiter(rpm=60, burst=3)
        assert limiter.try_acquire("s1")
        assert limiter.try_acquire("s1")
        assert limiter.try_acquire("s1")

    def test_rejects_after_burst_exhausted(self):
        limiter = SessionRateLimiter(rpm=60, burst=2)
        assert limiter.try_acquire("s1")
        assert limiter.try_acquire("s1")
        assert not limiter.try_acquire("s1")

    def test_different_sessions_independent(self):
        limiter = SessionRateLimiter(rpm=60, burst=1)
        assert limiter.try_acquire("s1")
        assert not limiter.try_acquire("s1")
        assert limiter.try_acquire("s2")

    def test_refills_over_time(self):
        limiter = SessionRateLimiter(rpm=6000, burst=1)
        assert limiter.try_acquire("s1")
        assert not limiter.try_acquire("s1")
        time.sleep(0.02)
        assert limiter.try_acquire("s1")

    def test_evicts_oldest_sessions(self):
        limiter = SessionRateLimiter(rpm=60, burst=5, max_sessions=3)
        limiter.try_acquire("s1")
        limiter.try_acquire("s2")
        limiter.try_acquire("s3")
        limiter.try_acquire("s4")
        assert len(limiter._buckets) == 3
        assert "s1" not in limiter._buckets

    def test_burst_caps_token_accumulation(self):
        limiter = SessionRateLimiter(rpm=60000, burst=2)
        limiter.try_acquire("s1")
        time.sleep(0.1)
        assert limiter.try_acquire("s1")
        assert limiter.try_acquire("s1")
        assert not limiter.try_acquire("s1")
