from __future__ import annotations

import pytest

from echo_agent.agent.tools.base import ToolExecutionContext
from echo_agent.agent.tools.cronjob import CronjobTool
from echo_agent.bus.queue import MessageBus
from echo_agent.scheduler.delivery import build_scheduled_job_handler, target_from_session_key
from echo_agent.scheduler.service import ScheduledJob, Scheduler


def test_target_from_session_key_supports_gateway_sessions() -> None:
    assert target_from_session_key("weixin:wxid_123") == ("weixin", "wxid_123")
    assert target_from_session_key("gateway:weixin:wxid:123") == ("gateway:weixin", "wxid:123")


@pytest.mark.asyncio
async def test_scheduler_job_handler_publishes_cron_event_to_source_chat() -> None:
    bus = MessageBus()
    handler = build_scheduled_job_handler(bus)
    job = ScheduledJob(
        id="job1",
        name="drink water",
        payload={
            "command": "提醒用户喝水",
            "source_session_key": "weixin:wxid_123",
        },
    )

    await handler(job)

    event = await bus._inbound_queue.get()
    assert event.channel == "weixin"
    assert event.sender_id == "cron"
    assert event.chat_id == "wxid_123"
    assert event.session_key == "weixin:wxid_123"
    assert event.text == "提醒用户喝水"
    assert event.metadata["job_id"] == "job1"
    assert event.metadata["deliver_channel"] == "weixin"
    assert event.metadata["deliver_chat_id"] == "wxid_123"


@pytest.mark.asyncio
async def test_cronjob_create_records_current_chat_delivery(tmp_path) -> None:
    scheduler = Scheduler(store_path=tmp_path / "scheduler.json")
    tool = CronjobTool(scheduler)

    result = await tool.execute(
        {
            "action": "create",
            "name": "drink water",
            "schedule": "*/5 * * * *",
            "command": "提醒用户喝水",
        },
        ToolExecutionContext(session_key="weixin:wxid_123"),
    )

    assert result.success
    jobs = scheduler.list_jobs()
    assert len(jobs) == 1
    assert jobs[0].payload == {
        "command": "提醒用户喝水",
        "source_session_key": "weixin:wxid_123",
        "deliver_channel": "weixin",
        "deliver_chat_id": "wxid_123",
    }
