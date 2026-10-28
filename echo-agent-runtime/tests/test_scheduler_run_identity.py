"""Scheduler run identity spans enqueue, execution, and terminal writeback."""

import asyncio

import pytest

from echo_agent.bus.queue import MessageBus
from echo_agent.scheduler.delivery import build_scheduled_job_handler
from echo_agent.scheduler.service import ScheduledJob, Scheduler


@pytest.mark.asyncio
async def test_queued_run_prevents_manual_and_timer_overlap(tmp_path):
    bus = MessageBus()
    scheduler = Scheduler(tmp_path / "scheduler.json", on_job=build_scheduled_job_handler(bus))
    job = scheduler.add_job(ScheduledJob(id="slow", interval_ms=1, payload={"command": "work"}))
    assert await scheduler.trigger_job(job.id)
    assert not await scheduler.trigger_job(job.id)
    assert job.run_count == 1
    event = await bus._inbound_queue.get()
    assert event.metadata["run_id"] == scheduler.get_run_history(job.id)[0]["run_id"]


@pytest.mark.asyncio
async def test_old_run_outcome_cannot_change_new_run(tmp_path):
    scheduler = Scheduler(tmp_path / "scheduler.json", on_job=build_scheduled_job_handler(MessageBus()))
    job = scheduler.add_job(ScheduledJob(id="job", interval_ms=1, payload={"command": "work"}))
    await scheduler.trigger_job(job.id)
    first = scheduler.get_run_history(job.id)[0]["run_id"]
    await scheduler.record_run_outcome(job.id, "completed", run_id=first)
    await scheduler.trigger_job(job.id)
    second = scheduler.get_run_history(job.id)[0]["run_id"]
    await scheduler.record_run_outcome(job.id, "error", "late old failure", run_id=first)
    assert job.last_status == "queued"
    assert job.last_error == ""
    history = scheduler.get_run_history(job.id)
    assert [(h["run_id"], h["status"]) for h in history] == [(second, "queued"), (first, "completed")]


@pytest.mark.asyncio
async def test_fast_completion_before_enqueue_handler_returns_is_not_lost(tmp_path):
    async def handler(job):
        await scheduler.record_run_outcome(job.id, "completed", run_id=job.run_id)
        return "queued"

    scheduler = Scheduler(tmp_path / "scheduler.json", on_job=handler)
    job = scheduler.add_job(ScheduledJob(id="fast", interval_ms=1))
    await scheduler.trigger_job(job.id)
    assert job.last_status == "completed"
    assert scheduler.get_run_history(job.id)[0]["status"] == "completed"


@pytest.mark.asyncio
async def test_restart_records_queued_run_as_interrupted_without_reexecuting(tmp_path):
    store = tmp_path / "scheduler.json"
    scheduler = Scheduler(store, on_job=build_scheduled_job_handler(MessageBus()))
    job = scheduler.add_job(ScheduledJob(id="lost", interval_ms=600_000, payload={"command": "work"}))
    await scheduler.trigger_job(job.id)
    restored = Scheduler(store)
    await restored.start()
    try:
        history = restored.get_run_history(job.id)
        assert history[0]["status"] == "interrupted"
        assert history[0]["completed_ts"] is not None
        assert restored.get_job(job.id).run_count == 1
        assert not restored.get_job(job.id).active_run_id
    finally:
        await restored.stop()


@pytest.mark.asyncio
async def test_concurrent_manual_triggers_admit_one_run(tmp_path):
    scheduler = Scheduler(tmp_path / "scheduler.json", on_job=build_scheduled_job_handler(MessageBus()))
    job = scheduler.add_job(ScheduledJob(id="race", interval_ms=1, payload={"command": "work"}))
    results = await asyncio.gather(*(scheduler.trigger_job(job.id) for _ in range(10)))
    assert sum(results) == 1
    assert job.run_count == 1


@pytest.mark.asyncio
async def test_scheduler_capacity_spans_dispatched_turn_not_just_enqueue(tmp_path):
    scheduler = Scheduler(tmp_path / "scheduler.json", on_job=build_scheduled_job_handler(MessageBus()),
                          max_concurrent=1)
    first = scheduler.add_job(ScheduledJob(id="first", interval_ms=1, payload={"command": "work"}))
    second = scheduler.add_job(ScheduledJob(id="second", interval_ms=1, payload={"command": "work"}))
    assert await scheduler.trigger_job(first.id)
    assert not await scheduler.trigger_job(second.id)
    await scheduler.record_run_outcome(first.id, "completed", run_id=first.active_run_id)
    assert await scheduler.trigger_job(second.id)


@pytest.mark.asyncio
async def test_real_agent_incomplete_turn_is_not_a_completed_cron_run(tmp_path):
    from tests.test_agent_loop_core import _make_agent_loop, _StubProvider
    from echo_agent.models.provider import LLMResponse

    agent, bus, _ = _make_agent_loop(
        tmp_path, _StubProvider([LLMResponse(content="provider failed", finish_reason="error")]),
    )
    scheduler = Scheduler(tmp_path / "scheduler.json", on_job=build_scheduled_job_handler(bus))
    agent._scheduler = scheduler
    agent._running = True

    async def receive(event):
        return None

    bus.subscribe_outbound_global(receive)
    job = scheduler.add_job(ScheduledJob(id="unfinished", interval_ms=60_000, payload={"command": "work"}))
    await scheduler.trigger_job(job.id)
    await agent._on_inbound(await bus._inbound_queue.get())
    assert job.last_status == "incomplete"
    assert scheduler.get_run_history(job.id)[0]["status"] == "incomplete"
