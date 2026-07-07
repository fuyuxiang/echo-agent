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
async def test_cron_event_carries_typed_trust_fields() -> None:
    """cron 事件把信任信号放在 InboundEvent 的类型化字段上,而非 metadata。"""
    from echo_agent.scheduler.delivery import inbound_event_from_job

    job = ScheduledJob(
        id="j", name="n",
        payload={"command": "x", "source_session_key": "weixin:w"},
    )
    event = inbound_event_from_job(job)
    assert event.unattended is True
    assert event.cron_authorized is True
    # 且不再泄漏到 metadata(否则又回到可被外部通道伪造的老路)
    assert "_unattended" not in event.metadata
    assert "_cron_authorized" not in event.metadata


@pytest.mark.asyncio
async def test_unattended_authorized_false_disables_cron_grant() -> None:
    """payload 显式 unattended_authorized=False 时,cron_authorized 应为 False。"""
    from echo_agent.scheduler.delivery import inbound_event_from_job

    job = ScheduledJob(
        id="j", name="n",
        payload={"command": "x", "source_session_key": "weixin:w", "unattended_authorized": False},
    )
    event = inbound_event_from_job(job)
    assert event.unattended is True
    assert event.cron_authorized is False


@pytest.mark.asyncio
async def test_record_run_outcome_overwrites_queued(tmp_path) -> None:
    """端到端完成后回写 completed,覆盖入队时的 queued。"""
    scheduler = Scheduler(
        store_path=tmp_path / "scheduler.json",
        on_job=build_scheduled_job_handler(MessageBus()),
    )
    job = ScheduledJob(
        id="jx", name="b", cron_expr="*/5 * * * *",
        payload={"command": "x", "source_session_key": "weixin:w"},
    )
    scheduler.add_job(job)
    await scheduler._run_job(job)
    assert job.last_status == "queued"  # 入队瞬间

    await scheduler.record_run_outcome("jx", "completed")
    assert job.last_status == "completed"
    assert job.last_error == ""

    await scheduler.record_run_outcome("jx", "error", "boom")
    assert job.last_status == "error"
    assert job.last_error == "boom"


@pytest.mark.asyncio
async def test_record_run_outcome_noop_on_missing_job(tmp_path) -> None:
    """job 已被清理(run-once 后)时,迟到的回写不得复活状态,应静默 no-op。"""
    scheduler = Scheduler(store_path=tmp_path / "scheduler.json")
    # 不存在的 job_id
    await scheduler.record_run_outcome("ghost", "completed")
    assert scheduler.list_jobs() == []


@pytest.mark.asyncio
async def test_loop_writeback_routes_only_cron_events_with_job_id() -> None:
    """AgentLoop._record_cron_outcome 的路由:仅对带 job_id 的 CRON 事件回写,
    普通消息 / 无 job_id 的巡检事件不触发。用轻量替身直接验证方法逻辑,
    避免构造完整 AgentLoop。"""
    from types import SimpleNamespace

    from echo_agent.agent.loop import AgentLoop
    from echo_agent.bus.events import ContentBlock, ContentType, EventType, InboundEvent

    calls: list[tuple[str, str, str]] = []

    class _FakeScheduler:
        async def record_run_outcome(self, job_id, status, error=""):
            calls.append((job_id, status, error))

    stub = SimpleNamespace(_scheduler=_FakeScheduler())
    record = AgentLoop._record_cron_outcome.__get__(stub, AgentLoop)

    def _evt(event_type, meta):
        return InboundEvent(
            event_type=event_type, channel="weixin", sender_id="cron", chat_id="c",
            content=[ContentBlock(type=ContentType.TEXT, text="x")], metadata=meta,
        )

    # 1) CRON + job_id → 回写
    await record(_evt(EventType.CRON, {"job_id": "j1"}), "completed")
    # 2) CRON 但无 job_id(巡检事件)→ 不回写
    await record(_evt(EventType.CRON, {"_inspection": True}), "completed")
    # 3) 普通消息即便带 job_id → 不回写(非 CRON)
    await record(_evt(EventType.MESSAGE, {"job_id": "j2"}), "completed")

    assert calls == [("j1", "completed", "")]


@pytest.mark.asyncio
async def test_loop_writeback_noop_without_scheduler() -> None:
    """未装配 scheduler 时,回写静默跳过,不抛异常。"""
    from types import SimpleNamespace

    from echo_agent.agent.loop import AgentLoop
    from echo_agent.bus.events import ContentBlock, ContentType, EventType, InboundEvent

    stub = SimpleNamespace(_scheduler=None)
    record = AgentLoop._record_cron_outcome.__get__(stub, AgentLoop)
    event = InboundEvent(
        event_type=EventType.CRON, channel="weixin", sender_id="cron", chat_id="c",
        content=[ContentBlock(type=ContentType.TEXT, text="x")], metadata={"job_id": "j"},
    )
    await record(event, "completed")  # 不抛异常即通过


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
