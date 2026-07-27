"""Scheduler.update_job: edits must recompute the firing schedule.

Editing used to happen in the API layer (assign fields, call save_state()), which
left next_run_ms pointing at an occurrence of the *previous* cron expression — a
new expression only took effect after the old pending time had elapsed.
"""

from __future__ import annotations

from pathlib import Path

from echo_agent.scheduler.service import (
    ScheduledJob,
    Scheduler,
    TriggerKind,
    _now_ms,
)


def _sched(tmp_path: Path) -> Scheduler:
    return Scheduler(store_path=tmp_path / "tasks.json")


def _job(**kw) -> ScheduledJob:
    base = dict(id="j1", name="j", trigger=TriggerKind.CRON, cron_expr="0 9 * * *")
    base.update(kw)
    return ScheduledJob(**base)


def test_changing_expression_recomputes_next_run(tmp_path: Path) -> None:
    sched = _sched(tmp_path)
    sched.add_job(_job(cron_expr="0 9 * * *"))
    before = sched.get_job("j1").next_run_ms

    updated = sched.update_job("j1", cron_expr="*/5 * * * *")

    assert updated is not None
    assert updated.cron_expr == "*/5 * * * *"
    assert updated.next_run_ms != before
    # The every-5-minutes expression must fire within the next 5 minutes.
    assert updated.next_run_ms - _now_ms() <= 5 * 60 * 1000 + 1000


def test_unchanged_expression_keeps_next_run(tmp_path: Path) -> None:
    """A rename must not shift the schedule — recomputing on every edit would
    silently push the next firing later each time the name is touched."""
    sched = _sched(tmp_path)
    sched.add_job(_job())
    before = sched.get_job("j1").next_run_ms

    updated = sched.update_job("j1", name="renamed", cron_expr="0 9 * * *")

    assert updated.name == "renamed"
    assert updated.next_run_ms == before


def test_resume_recomputes_from_now_instead_of_firing_immediately(tmp_path: Path) -> None:
    """Re-enabling a long-paused job must not fire the missed occurrences.

    A stale past next_run_ms would make the very next tick dispatch the job, so
    "pause for a week, then resume" fires at once. start() already recomputes
    from now for downtime; resume follows the same rule.
    """
    sched = _sched(tmp_path)
    sched.add_job(_job())
    job = sched.get_job("j1")
    job.enabled = False
    # Pretend the pause outlasted several occurrences.
    job.next_run_ms = _now_ms() - 7 * 24 * 60 * 60 * 1000

    updated = sched.update_job("j1", enabled=True)

    assert updated.enabled is True
    assert updated.next_run_ms > _now_ms()


def test_pausing_leaves_stored_time_alone(tmp_path: Path) -> None:
    """Disabling needs no new time: the tick loop skips disabled jobs, and the
    resume path recomputes anyway."""
    sched = _sched(tmp_path)
    sched.add_job(_job())
    before = sched.get_job("j1").next_run_ms

    updated = sched.update_job("j1", enabled=False)

    assert updated.enabled is False
    assert updated.next_run_ms == before


def test_update_persists(tmp_path: Path) -> None:
    store = tmp_path / "tasks.json"
    sched = Scheduler(store_path=store)
    sched.add_job(_job())
    sched.update_job("j1", name="renamed", cron_expr="*/5 * * * *")

    reloaded = Scheduler(store_path=store)
    job = reloaded.get_job("j1")
    assert job.name == "renamed"
    assert job.cron_expr == "*/5 * * * *"


def test_update_missing_job_returns_none(tmp_path: Path) -> None:
    sched = _sched(tmp_path)
    assert sched.update_job("nope", name="x") is None


def test_uncomputable_expression_clears_next_run(tmp_path: Path) -> None:
    """A cron expression that yields no next occurrence must leave next_run_ms
    empty rather than keeping the old one — otherwise the job fires on a
    schedule it no longer has."""
    sched = _sched(tmp_path)
    sched.add_job(_job())
    updated = sched.update_job("j1", cron_expr="not a cron expr")
    assert updated.next_run_ms is None
