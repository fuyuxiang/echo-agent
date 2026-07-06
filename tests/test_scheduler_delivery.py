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
async def test_handler_returns_queued_not_success() -> None:
    """publish_inbound 只代表入队，handler 应返回 'queued' 而非声称已完成。"""
    bus = MessageBus()
    handler = build_scheduled_job_handler(bus)
    job = ScheduledJob(
        id="job2",
        name="briefing",
        payload={"command": "早报", "source_session_key": "weixin:wxid_9"},
    )
    status = await handler(job)
    assert status == "queued"


@pytest.mark.asyncio
async def test_run_job_records_queued_status(tmp_path) -> None:
    """_run_job 应如实记录 handler 返回的状态，而不是硬编码 success。"""
    scheduler = Scheduler(
        store_path=tmp_path / "scheduler.json",
        on_job=build_scheduled_job_handler(MessageBus()),
    )
    job = ScheduledJob(
        id="job3", name="b", cron_expr="*/5 * * * *",
        payload={"command": "x", "source_session_key": "weixin:w"},
    )
    await scheduler._run_job(job)
    assert job.last_status == "queued"
    assert job.run_count == 1


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


@pytest.mark.asyncio
async def test_cronjob_create_warns_when_no_delivery_target(tmp_path) -> None:
    """无法解析投递目标时,创建仍成功但必须显式告警,不静默吞产出。"""
    scheduler = Scheduler(store_path=tmp_path / "scheduler.json")
    tool = CronjobTool(scheduler)

    # 会话键无法推导出 channel/chat_id(空键),且未显式指定 target
    result = await tool.execute(
        {
            "action": "create",
            "name": "orphan",
            "schedule": "*/5 * * * *",
            "command": "生成早报",
        },
        ToolExecutionContext(session_key=""),
    )

    assert result.success
    assert "警告" in result.output or "⚠️" in result.output
