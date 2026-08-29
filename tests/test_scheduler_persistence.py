"""Tests for Scheduler persistence atomicity and tick concurrency."""

from __future__ import annotations

import asyncio
import json
import os
from pathlib import Path

import pytest

import echo_agent.scheduler.service as scheduler_service
from echo_agent.scheduler.service import (
    ScheduledJob,
    Scheduler,
    TriggerKind,
    _now_ms,
)


# ── P0-4: atomic write ───────────────────────────────────────────────────────


def test_release_lock_closes_descriptor_when_unlock_fails(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch,
) -> None:
    """An unlock syscall failure must not leak the scheduler lock descriptor."""

    class FailingFcntl:
        LOCK_UN = 1

        @staticmethod
        def flock(_fd, _operation) -> None:
            raise OSError("unlock failed")

    class LockFile:
        closed = False

        def close(self) -> None:
            self.closed = True

    monkeypatch.setattr(scheduler_service, "fcntl", FailingFcntl())
    scheduler = Scheduler(store_path=tmp_path / "tasks.json")
    lock_file = LockFile()

    scheduler._release_lock(lock_file)

    assert lock_file.closed is True


def test_save_uses_atomic_replace(tmp_path: Path) -> None:
    """A crash mid-write must not leave the tasks JSON truncated. The save
    path uses tempfile + os.replace so partial files never appear under
    the canonical path."""
    store_path = tmp_path / "tasks.json"
    sched = Scheduler(store_path=store_path)
    job = ScheduledJob(
        id="j1", name="t", trigger=TriggerKind.INTERVAL, interval_ms=1000,
    )
    sched.add_job(job)

    # File is well-formed JSON.
    raw = store_path.read_text(encoding="utf-8")
    parsed = json.loads(raw)
    assert any(j["id"] == "j1" for j in parsed["jobs"])

    # No leftover .tmp files in the directory after save.
    leftover_tmp = [p for p in tmp_path.iterdir() if p.suffix == ".tmp"]
    assert leftover_tmp == []


def test_save_does_not_corrupt_on_replace_failure(tmp_path: Path, monkeypatch) -> None:
    """If os.replace fails, the original file must remain untouched (we can't
    leave behind a half-written file at the canonical path)."""
    store_path = tmp_path / "tasks.json"
    sched = Scheduler(store_path=store_path)
    sched.add_job(ScheduledJob(id="orig", name="o", trigger=TriggerKind.INTERVAL, interval_ms=1000))

    original = store_path.read_text(encoding="utf-8")

    real_replace = os.replace

    def boom(src, dst):
        raise OSError("simulated disk failure")

    monkeypatch.setattr("echo_agent.scheduler.service.os.replace", boom)

    with pytest.raises(OSError):
        sched.add_job(ScheduledJob(id="new", name="n", trigger=TriggerKind.INTERVAL, interval_ms=1000))

    # The canonical file must still parse and still contain the original job.
    after = store_path.read_text(encoding="utf-8")
    assert json.loads(after) == json.loads(original)

    # And no orphan .tmp file should remain in the directory.
    monkeypatch.setattr("echo_agent.scheduler.service.os.replace", real_replace)
    leftover_tmp = [p for p in tmp_path.iterdir() if p.suffix == ".tmp"]
    assert leftover_tmp == []


# ── P0-5: tick fan-out concurrency ───────────────────────────────────────────


@pytest.mark.asyncio
async def test_slow_job_does_not_block_other_due_jobs(tmp_path: Path) -> None:
    """Two cron jobs that come due at the same instant must execute in
    parallel (bounded by max_concurrent), not serially. Prior to the fix
    a 5s-long job blocked every other job for the whole 5s."""
    started: list[str] = []
    finished: list[str] = []
    barrier = asyncio.Event()

    async def on_job(job):
        started.append(job.id)
        if job.id == "slow":
            # Wait for the fast job to also start AND finish before unblocking.
            await asyncio.wait_for(barrier.wait(), timeout=2.0)
        finished.append(job.id)
        return None

    sched = Scheduler(store_path=tmp_path / "tasks.json", on_job=on_job, max_concurrent=4)
    slow = ScheduledJob(id="slow", name="s", trigger=TriggerKind.INTERVAL, interval_ms=100)
    fast = ScheduledJob(id="fast", name="f", trigger=TriggerKind.INTERVAL, interval_ms=100)
    sched.add_job(slow)
    sched.add_job(fast)

    await sched.start()
    # start() recomputes next_run_ms — overwrite it AFTER start so both jobs
    # are due on the very first tick.
    now = _now_ms()
    sched._jobs["slow"].next_run_ms = now - 1
    sched._jobs["fast"].next_run_ms = now - 1
    try:
        # Wait until the fast job has finished — proves it didn't have to
        # wait for `slow` to release the tick loop.
        loop = asyncio.get_event_loop()
        deadline = loop.time() + 6.0
        while "fast" not in finished and loop.time() < deadline:
            await asyncio.sleep(0.05)
        assert "fast" in finished, f"started={started} finished={finished}"
        # And the slow job is still in flight.
        assert "slow" in started
        assert "slow" not in finished
        # Now release the slow job and let everything wind down.
        barrier.set()
        deadline = loop.time() + 3.0
        while "slow" not in finished and loop.time() < deadline:
            await asyncio.sleep(0.05)
        assert "slow" in finished
    finally:
        await sched.stop()


@pytest.mark.asyncio
async def test_inflight_job_not_dispatched_twice(tmp_path: Path) -> None:
    """If a job is still running when its next tick fires, it must not be
    dispatched a second time — otherwise an interval shorter than the
    runtime would pile up duplicate runs."""
    inflight = asyncio.Event()
    release = asyncio.Event()
    starts: list[str] = []

    async def on_job(job):
        starts.append(job.id)
        inflight.set()
        await release.wait()
        return None

    sched = Scheduler(store_path=tmp_path / "tasks.json", on_job=on_job, max_concurrent=4)
    now = _now_ms()
    sched.add_job(ScheduledJob(
        id="repeat", name="r", trigger=TriggerKind.INTERVAL, interval_ms=10,
        next_run_ms=now - 1000,
    ))

    await sched.start()
    try:
        await asyncio.wait_for(inflight.wait(), timeout=2.0)
        # Let several tick periods elapse; a buggy dispatcher would spawn
        # additional runs while the first is still blocked.
        await asyncio.sleep(1.5)
        assert starts.count("repeat") == 1
        release.set()
    finally:
        await sched.stop()
