import pytest

from echo_agent.scheduler.delivery import build_scheduled_job_handler
from echo_agent.scheduler.service import ScheduledJob


class _FakeScheduler:
    def __init__(self, jobs):
        self._jobs = jobs

    def list_jobs(self):
        return self._jobs


class _Job:
    def __init__(self, payload):
        self.payload = payload
        self.id = "j1"
        self.name = "n"


class _Bus:
    def __init__(self):
        self.published = []

    async def publish_inbound(self, event):
        self.published.append(event)
        return True


@pytest.mark.asyncio
async def test_handler_routes_inspection_tick_to_runner():
    calls = []

    async def _runner():
        calls.append(1)

    handler = build_scheduled_job_handler(_Bus(), inspection_runner=_runner)
    await handler(_Job({"_inspection_tick": True}))
    assert calls == [1]  # runner invoked, not the command path


@pytest.mark.asyncio
async def test_handler_routes_normal_job_to_command_path():
    bus = _Bus()
    handler = build_scheduled_job_handler(bus, inspection_runner=None)
    await handler(_Job({"command": "hello", "source_session_key": "weixin:room1"}))
    assert len(bus.published) == 1  # normal command still published as inbound
    assert bus.published[0].content[0].text == "hello"


def _dedup_should_add(scheduler):
    """Mirror app.py restart-dedup guard: add only when no tick job exists yet."""
    return not any(
        j.name == "__inspection_tick__" for j in scheduler.list_jobs()
    )


def test_restart_dedup_skips_when_tick_job_exists():
    """已存在同名 __inspection_tick__ job 时，去重条件为 False（不重复注册）。"""
    existing = ScheduledJob(name="__inspection_tick__")
    scheduler = _FakeScheduler([existing])
    assert _dedup_should_add(scheduler) is False


def test_restart_dedup_adds_when_no_tick_job():
    """无同名 job 时，去重条件为 True（会注册节拍器 job）。"""
    scheduler = _FakeScheduler([])
    assert _dedup_should_add(scheduler) is True
