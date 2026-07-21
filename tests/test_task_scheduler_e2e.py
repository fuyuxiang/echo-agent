# tests/test_task_scheduler_e2e.py
"""B 组契约级端到端:Cron 创建校验、分发回滚、孤儿回收、并发终态唯一。"""
import asyncio
from types import SimpleNamespace
from unittest.mock import MagicMock

import pytest
from aiohttp import web
from aiohttp.test_utils import TestServer, TestClient

from echo_agent.agent.loop import AgentLoop
from echo_agent.bus.events import ContentBlock, ContentType, EventType, InboundEvent
from echo_agent.gateway.api.cron_api import CronAPI
from echo_agent.storage.sqlite import SQLiteBackend
from echo_agent.tasks.dispatcher import TaskDispatcher, new_owner_id
from echo_agent.tasks.manager import TaskManager
from echo_agent.tasks.models import TaskStatus, _now_ms


class _FakeBus:
    def __init__(self, accept=True):
        self.accept = accept
        self.published = []

    async def publish_inbound(self, event):
        self.published.append(event)
        return self.accept


def _task_event(task_id):
    return InboundEvent(
        event_type=EventType.SYSTEM, channel="task", sender_id="dispatcher",
        chat_id=task_id, content=[ContentBlock(type=ContentType.TEXT, text="x")],
        metadata={"task_id": task_id},
    )


# ① Cron 创建校验:空 command 被 400 拒绝(契约起点)。
@pytest.mark.asyncio
async def test_e2e_cron_create_rejects_empty_command():
    server = MagicMock()
    server._require_api_token = MagicMock(return_value=None)
    server._agent_loop = MagicMock(spec_set=AgentLoop)
    server._agent_loop.scheduler = MagicMock()
    api = CronAPI(server)

    app = web.Application()
    app.router.add_post("/api/v1/cron", api.create_job)
    async with TestClient(TestServer(app)) as client:
        resp = await client.post("/api/v1/cron", json={"name": "n", "cron_expr": "* * * * *"})
        assert resp.status == 400


# ① 带 command 的任务全链路走到终态 completed(dispatch→turn→writeback)。
@pytest.mark.asyncio
async def test_e2e_dispatched_task_reaches_completed(tmp_path):
    backend = SQLiteBackend(tmp_path / "db.sqlite")
    await backend.initialize()
    manager = TaskManager(backend)
    bus = _FakeBus()
    dispatcher = TaskDispatcher(bus, manager, owner_id=new_owner_id(), max_concurrent=2, lease_ttl_ms=60000)
    manager.add_terminal_listener(dispatcher._on_task_terminal)

    task = await manager.create(title="巡检磁盘", description="cmd")
    await manager.transition(task.id, TaskStatus.QUEUED)

    d = asyncio.create_task(dispatcher._dispatch(task))
    await asyncio.sleep(0.05)
    assert len(bus.published) == 1
    assert (await manager.get(task.id)).status == TaskStatus.RUNNING

    # 模拟 AgentLoop 收尾:turn 干净完成 → SUCCESS(经 REVIEW hop)。
    stub = SimpleNamespace(_task_manager=manager, _workflow_engine=None)
    record = AgentLoop._record_task_outcome.__get__(stub, AgentLoop)
    await record(_task_event(task.id), "completed")

    await asyncio.wait_for(d, timeout=1.0)
    assert (await manager.get(task.id)).status == TaskStatus.SUCCESS
    await backend.close()


# ② transition 后 publish 前抛异常 → 回 QUEUED。
@pytest.mark.asyncio
async def test_e2e_publish_failure_rolls_back_to_queued(tmp_path):
    backend = SQLiteBackend(tmp_path / "db.sqlite")
    await backend.initialize()
    manager = TaskManager(backend)
    bus = _FakeBus()

    async def _raise(event):
        raise RuntimeError("bus down after transition")

    bus.publish_inbound = _raise
    dispatcher = TaskDispatcher(bus, manager, owner_id=new_owner_id())
    task = await manager.create(title="t")
    await manager.transition(task.id, TaskStatus.QUEUED)

    await dispatcher._dispatch(task)
    assert (await manager.get(task.id)).status == TaskStatus.QUEUED
    await backend.close()


# ③ 新实例扫到过期 lease 的 RUNNING → requeue。
@pytest.mark.asyncio
async def test_e2e_new_instance_reclaims_expired_running(tmp_path):
    backend = SQLiteBackend(tmp_path / "db.sqlite")
    await backend.initialize()
    manager = TaskManager(backend)

    task = await manager.create(title="stranded")
    await manager.transition(task.id, TaskStatus.QUEUED)
    await manager.transition(task.id, TaskStatus.RUNNING)
    # 旧实例遗留:owner=OLD,租约已过期。
    await manager.set_running_context(task.id, "task:x", "evt", owner_id="OLD", lease_ttl_ms=-1)

    reclaimed = await manager.reclaim_expired_running(current_owner_id="NEW", now_ms=_now_ms())
    assert task.id in reclaimed
    assert (await manager.get(task.id)).status == TaskStatus.QUEUED
    await backend.close()


# ④ 并发 cancel + complete → 终态唯一。
@pytest.mark.asyncio
async def test_e2e_concurrent_cancel_and_complete_terminal_unique(tmp_path):
    backend = SQLiteBackend(tmp_path / "db.sqlite")
    await backend.initialize()
    manager = TaskManager(backend)
    task = await manager.create(title="race")
    await manager.transition(task.id, TaskStatus.QUEUED)
    await manager.transition(task.id, TaskStatus.RUNNING)

    async def cancel():
        try:
            return await manager.transition(task.id, TaskStatus.CANCELLED)
        except Exception:
            return None

    async def review_then_success():
        try:
            await manager.transition(task.id, TaskStatus.REVIEW)
            return await manager.transition(task.id, TaskStatus.SUCCESS)
        except Exception:
            return None

    await asyncio.gather(cancel(), review_then_success())
    final = await manager.get(task.id)
    assert final.status in (TaskStatus.CANCELLED, TaskStatus.SUCCESS)
    # 终态唯一且不可再迁移(CAS 未把已终态又翻成另一个终态)。
    from echo_agent.tasks.models import VALID_TASK_TRANSITIONS
    assert VALID_TASK_TRANSITIONS[final.status] == set()
    await backend.close()
