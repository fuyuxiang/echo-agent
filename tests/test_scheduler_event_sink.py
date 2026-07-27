"""Tests for the Scheduler → dashboard WS event sink.

The dashboard's `cron` channel existed in the WS channel map with nothing
emitting into it, so the cron page could only ever show state as of its last
manual load. These cover the contract the page now relies on: every run outcome
emits, and a broken subscriber can never affect the run itself.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any

import pytest

from echo_agent.scheduler.service import ScheduledJob, Scheduler, TriggerKind


def _job(job_id: str = "j1") -> ScheduledJob:
    return ScheduledJob(
        id=job_id, name="t", trigger=TriggerKind.INTERVAL, interval_ms=1000,
    )


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
