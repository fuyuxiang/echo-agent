"""Tests for the event-loop watchdog and restart circuit breaker.

The watchdog's kill path is exercised without touching the real process: the
clock is a controllable fake and exit_fn is a recorder, so we assert on
*decisions* (warn / kill / suppressed) deterministically, never on wall-clock.
"""

from __future__ import annotations

import asyncio

import pytest

from echo_agent.observability.loop_watchdog import LoopWatchdog
from echo_agent.observability.restart_guard import RestartGuard


class FakeClock:
    def __init__(self) -> None:
        self.t = 1000.0

    def __call__(self) -> float:
        return self.t

    def advance(self, dt: float) -> None:
        self.t += dt


def _make(tmp_path, clock, exits, **kw):
    guard = RestartGuard(tmp_path / "restarts.json", max_restarts=kw.pop("max_restarts", 5))
    return LoopWatchdog(
        warn_seconds=kw.pop("warn_seconds", 5.0),
        kill_seconds=kw.pop("kill_seconds", 30.0),
        check_interval_seconds=kw.pop("check_interval_seconds", 0.01),
        heartbeat_interval_seconds=kw.pop("heartbeat_interval_seconds", 0.01),
        restart_guard=guard,
        supervised=kw.pop("supervised", True),
        exit_fn=lambda code: exits.append(code),
        clock=clock,
        **kw,
    )


class TestMonitorDecisions:
    def test_no_action_when_fresh(self, tmp_path):
        clock, exits = FakeClock(), []
        wd = _make(tmp_path, clock, exits)
        # Heartbeat just stamped; staleness 0 -> neither warn nor kill.
        wd._last_beat = clock()
        stale = clock() - wd._last_beat
        assert stale < wd._warn
        assert exits == []

    def test_warn_then_kill_thresholds(self, tmp_path):
        clock, exits = FakeClock(), []
        wd = _make(tmp_path, clock, exits, warn_seconds=5, kill_seconds=30)
        wd._last_beat = 1000.0

        # Below warn: nothing.
        clock.t = 1003.0
        assert (clock() - wd._last_beat) < wd._warn

        # Cross warn: on_warn dumps + logs, does not exit.
        clock.t = 1006.0
        wd._on_warn(clock() - wd._last_beat)
        assert exits == []

        # Cross kill: on_kill records restart + exits with code 75.
        clock.t = 1031.0
        wd._on_kill(clock() - wd._last_beat)
        assert exits == [75]
        assert wd._guard.recent_count() == 1

    def test_unsupervised_kill_does_not_exit(self, tmp_path):
        clock, exits = FakeClock(), []
        wd = _make(tmp_path, clock, exits, supervised=False)
        wd._on_kill(99.0)
        assert exits == []  # warn-only: never exits when unsupervised


class TestRestartGuard:
    def test_trips_after_max(self, tmp_path):
        clock = FakeClock()
        g = RestartGuard(tmp_path / "r.json", max_restarts=3, now=clock)
        assert not g.is_tripped()
        for _ in range(3):
            g.record_restart()
        assert g.is_tripped()
        assert g.recent_count() == 3

    def test_prunes_outside_window(self, tmp_path):
        clock = FakeClock()
        g = RestartGuard(tmp_path / "r.json", max_restarts=3, window_seconds=100, now=clock)
        g.record_restart()
        g.record_restart()
        assert g.recent_count() == 2
        clock.advance(200)  # both fall outside the 100s window
        assert g.recent_count() == 0
        assert not g.is_tripped()

    def test_corrupt_file_is_ignored(self, tmp_path):
        p = tmp_path / "r.json"
        p.write_text("not json{{{")
        g = RestartGuard(p, max_restarts=3)
        assert g.recent_count() == 0  # unreadable -> treated as empty


class TestArming:
    @pytest.mark.asyncio
    async def test_refuses_to_arm_when_tripped(self, tmp_path):
        clock, exits = FakeClock(), []
        # Pre-trip the guard.
        g = RestartGuard(tmp_path / "restarts.json", max_restarts=1)
        g.record_restart()
        wd = LoopWatchdog(
            restart_guard=g, supervised=True,
            exit_fn=lambda c: exits.append(c), clock=clock,
            check_interval_seconds=0.01, heartbeat_interval_seconds=0.01,
        )
        wd.start()
        # Not armed: no monitor thread, no heartbeat task.
        assert wd._thread is None
        assert wd._hb_task is None
        await wd.stop()

    @pytest.mark.asyncio
    async def test_heartbeat_keeps_stamp_fresh(self, tmp_path):
        # With a real clock and a live loop, the heartbeat must keep advancing
        # _last_beat — i.e. a healthy loop is never flagged. Uses real monotonic
        # via default clock but asserts on staleness, not wall-clock duration.
        import time
        exits: list[int] = []
        guard = RestartGuard(tmp_path / "restarts.json")
        wd = LoopWatchdog(
            warn_seconds=100, kill_seconds=200,
            check_interval_seconds=0.02, heartbeat_interval_seconds=0.01,
            restart_guard=guard, supervised=True,
            exit_fn=lambda c: exits.append(c),
        )
        wd.start()
        before = wd._last_beat
        await asyncio.sleep(0.05)  # let the heartbeat tick a few times
        after = wd._last_beat
        await wd.stop()
        assert after >= before
        assert (time.monotonic() - after) < 1.0
        assert exits == []

    @pytest.mark.asyncio
    async def test_real_block_triggers_kill(self, tmp_path):
        # End-to-end: block the event loop thread synchronously and assert the
        # daemon watchdog observes the stalled heartbeat and fires exit_fn. This
        # is the direct proof that a frozen loop is detected and self-healed.
        import threading
        import time

        killed = threading.Event()
        exits: list[int] = []

        def fake_exit(code: int) -> None:
            exits.append(code)
            killed.set()

        guard = RestartGuard(tmp_path / "restarts.json")
        wd = LoopWatchdog(
            warn_seconds=0.05, kill_seconds=0.2,
            check_interval_seconds=0.02, heartbeat_interval_seconds=0.02,
            restart_guard=guard, supervised=True,
            exit_fn=fake_exit,
        )
        wd.start()
        # Synchronously block the loop thread — the heartbeat coroutine cannot
        # run, so its stamp goes stale and the daemon thread must react.
        time.sleep(0.5)
        # Yield so the loop resumes and we can tear down cleanly.
        await asyncio.sleep(0)
        assert killed.is_set()
        assert exits == [75]
        assert guard.recent_count() == 1
        await wd.stop()
