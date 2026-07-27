"""Tests for the Scheduler → dashboard WS event sink.

The dashboard's `cron` channel existed in the WS channel map with nothing
emitting into it, so the cron page could only ever show state as of its last
manual load. These cover the contract the page now relies on: every run outcome
emits, and a broken subscriber can never affect the run itself.
"""

from __future__ import annotations

import asyncio
from pathlib import Path
from typing import Any

import pytest

from echo_agent.scheduler import service
from echo_agent.scheduler.service import ScheduledJob, Scheduler, TriggerKind


def _job(job_id: str = "j1") -> ScheduledJob:
    return ScheduledJob(
        id=job_id, name="t", trigger=TriggerKind.INTERVAL, interval_ms=1000,
    )


async def _flush(sched: Scheduler) -> None:
    """Wait for queued events to reach the sink.

    Emission is deliberately off the job path now (bounded queue + drain task),
    so a test that asserts on delivery has to wait for the drain rather than
    assume the run awaited the sink."""
    queue = sched._event_queue
    if queue is not None:
        await asyncio.wait_for(queue.join(), timeout=5)


@pytest.mark.asyncio
async def test_run_job_emits_cron_run(tmp_path: Path) -> None:
    """A completed run pushes one cron_run carrying the full job dict, so the
    page can update a row without a follow-up fetch."""
    events: list[tuple[str, dict[str, Any]]] = []

    async def sink(event_type: str, payload: dict[str, Any]) -> None:
        events.append((event_type, payload))

    async def handler(job: ScheduledJob) -> str:
        return "queued"

    sched = Scheduler(store_path=tmp_path / "tasks.json", on_job=handler)
    sched.set_event_sink(sink)
    job = _job()
    sched.add_job(job)

    await sched._run_job(job)
    await _flush(sched)

    assert [e[0] for e in events] == ["cron_run"]
    payload = events[0][1]
    assert payload["id"] == "j1"
    assert payload["last_status"] == "queued"
    # The emit happens after the state writeback, so the payload is the saved
    # state rather than the pre-run one.
    assert payload["run_count"] == 1


@pytest.mark.asyncio
async def test_record_run_outcome_emits_terminal_status(tmp_path: Path) -> None:
    """The dispatch emit says "queued"; the page's "last result" column needs
    the terminal writeback to emit too, or a failed job stays green."""
    events: list[tuple[str, dict[str, Any]]] = []

    async def sink(event_type: str, payload: dict[str, Any]) -> None:
        events.append((event_type, payload))

    sched = Scheduler(store_path=tmp_path / "tasks.json")
    sched.set_event_sink(sink)
    sched.add_job(_job())

    await sched.record_run_outcome("j1", "error", "boom")
    await _flush(sched)

    assert len(events) == 1
    assert events[0][0] == "cron_run"
    assert events[0][1]["last_status"] == "error"
    assert events[0][1]["last_error"] == "boom"


@pytest.mark.asyncio
async def test_record_run_outcome_unknown_job_does_not_emit(tmp_path: Path) -> None:
    """A late writeback for a pruned run-once job is a no-op, and must not
    fabricate an event for a job the page no longer lists."""
    events: list[str] = []

    async def sink(event_type: str, payload: dict[str, Any]) -> None:
        events.append(event_type)

    sched = Scheduler(store_path=tmp_path / "tasks.json")
    sched.set_event_sink(sink)

    await sched.record_run_outcome("gone", "completed")
    await _flush(sched)

    assert events == []


@pytest.mark.asyncio
async def test_failing_sink_does_not_break_run(tmp_path: Path) -> None:
    """Emission is best-effort: a subscriber that raises must not surface as a
    job failure, since the run already happened and was persisted."""

    async def sink(event_type: str, payload: dict[str, Any]) -> None:
        raise RuntimeError("subscriber exploded")

    async def handler(job: ScheduledJob) -> str:
        return "queued"

    sched = Scheduler(store_path=tmp_path / "tasks.json", on_job=handler)
    sched.set_event_sink(sink)
    job = _job()
    sched.add_job(job)

    await sched._run_job(job)
    await _flush(sched)

    assert job.last_status == "queued"
    assert job.last_error == ""


@pytest.mark.asyncio
async def test_no_sink_is_a_noop(tmp_path: Path) -> None:
    """Headless runs and tests never wire a sink (only app.py does), so runs
    must work with the field left at None."""

    async def handler(job: ScheduledJob) -> str:
        return "queued"

    sched = Scheduler(store_path=tmp_path / "tasks.json", on_job=handler)
    job = _job()
    sched.add_job(job)

    await sched._run_job(job)

    assert job.last_status == "queued"


class TestEmitIsOffTheCriticalPath:
    """事件下沉为有界队列:订阅者再慢也不能拖住调度。

    原实现在 _execute_job 里直接 await sink(),而 job 在 _execute_job 返回前一直留在
    _inflight_jobs 里、下一个 tick 会跳过它。也就是说一个卡住的 dashboard WS 连接
    能实质性地压慢真实的任务执行频率。
    """

    @pytest.mark.asyncio
    async def test_slow_sink_does_not_delay_the_run(self, tmp_path: Path) -> None:
        entered = asyncio.Event()
        release = asyncio.Event()

        async def slow_sink(event_type: str, payload: dict[str, Any]) -> None:
            entered.set()
            await release.wait()

        async def handler(job: ScheduledJob) -> str:
            return "queued"

        sched = Scheduler(store_path=tmp_path / "tasks.json", on_job=handler)
        sched.set_event_sink(slow_sink)
        job = _job()
        sched.add_job(job)

        # 关键断言:sink 还堵在里面,_run_job 已经返回了。
        await asyncio.wait_for(sched._run_job(job), timeout=1)
        await asyncio.wait_for(entered.wait(), timeout=1)

        release.set()
        await _flush(sched)

    @pytest.mark.asyncio
    async def test_stuck_sink_is_bounded_by_a_timeout(
        self, tmp_path: Path, monkeypatch
    ) -> None:
        """一个永不返回的订阅者只赔掉它自己那条事件,后面的事件照发。"""
        monkeypatch.setattr(service, "_EVENT_SEND_TIMEOUT", 0.05)
        delivered: list[str] = []
        first = True

        async def sink(event_type: str, payload: dict[str, Any]) -> None:
            nonlocal first
            if first:
                first = False
                await asyncio.Event().wait()  # 永远不返回
            delivered.append(payload["last_status"])

        sched = Scheduler(store_path=tmp_path / "tasks.json")
        sched.set_event_sink(sink)
        sched.add_job(_job())

        await sched.record_run_outcome("j1", "error", "boom")
        await sched.record_run_outcome("j1", "completed")
        await _flush(sched)

        assert delivered == ["completed"]

    @pytest.mark.asyncio
    async def test_queue_overflow_drops_oldest_and_keeps_newest(
        self, tmp_path: Path, monkeypatch
    ) -> None:
        """队列满时丢最旧:dashboard 要的是最新结果,不是历史回放。"""
        monkeypatch.setattr(service, "_EVENT_QUEUE_MAX", 2)
        gate = asyncio.Event()
        delivered: list[str] = []

        async def sink(event_type: str, payload: dict[str, Any]) -> None:
            await gate.wait()
            delivered.append(payload["last_error"])

        sched = Scheduler(store_path=tmp_path / "tasks.json")
        sched.set_event_sink(sink)
        sched.add_job(_job())

        for marker in ("e1", "e2", "e3", "e4", "e5"):
            await sched.record_run_outcome("j1", "error", marker)
        assert sched._events_dropped > 0

        gate.set()
        await _flush(sched)

        # 最新的一条必须活着,总条数不超过队列容量(第一条可能已被 drain 取走)。
        assert "e5" in delivered
        assert len(delivered) <= 3

    @pytest.mark.asyncio
    async def test_stop_flushes_pending_events_and_ends_the_drain(
        self, tmp_path: Path
    ) -> None:
        """优雅停机时最后一次运行结果仍应送达,并且 drain 任务不能留在后台。"""
        delivered: list[str] = []

        async def sink(event_type: str, payload: dict[str, Any]) -> None:
            delivered.append(payload["last_status"])

        sched = Scheduler(store_path=tmp_path / "tasks.json")
        sched.set_event_sink(sink)
        sched.add_job(_job())
        await sched.start()

        await sched.record_run_outcome("j1", "completed")
        await sched.stop()

        assert delivered == ["completed"]
        assert sched._event_task is None

    @pytest.mark.asyncio
    async def test_stop_does_not_hang_on_a_stuck_subscriber(
        self, tmp_path: Path, monkeypatch
    ) -> None:
        """挂死的订阅者不能把停机拖住。"""
        monkeypatch.setattr(service, "_EVENT_SEND_TIMEOUT", 0.05)

        async def sink(event_type: str, payload: dict[str, Any]) -> None:
            await asyncio.Event().wait()

        sched = Scheduler(store_path=tmp_path / "tasks.json")
        sched.set_event_sink(sink)
        sched.add_job(_job())
        await sched.start()

        await sched.record_run_outcome("j1", "completed")
        await asyncio.wait_for(sched.stop(), timeout=2)

        assert sched._event_task is None
