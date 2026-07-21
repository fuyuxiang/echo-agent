import asyncio

import pytest

from echo_agent.storage.sqlite import SQLiteBackend
from echo_agent.tasks.manager import TaskManager
from echo_agent.tasks.models import TaskRecord, TaskCASConflict, TaskStatus, _now_ms


def test_task_record_has_lease_and_version_defaults():
    t = TaskRecord(title="t")
    assert t.owner_id == ""
    assert t.lease_until_ms is None
    assert t.attempt_id == ""
    assert t.version == 0


def test_task_record_roundtrip_preserves_new_fields():
    t = TaskRecord(title="t", owner_id="inst-1", lease_until_ms=123, attempt_id="a1", version=7)
    back = TaskRecord.from_dict(t.to_dict())
    assert (back.owner_id, back.lease_until_ms, back.attempt_id, back.version) == ("inst-1", 123, "a1", 7)


def test_now_ms_is_int_millis():
    assert isinstance(_now_ms(), int)
    assert _now_ms() > 1_600_000_000_000


def test_cas_conflict_is_exception():
    assert issubclass(TaskCASConflict, Exception)


@pytest.mark.asyncio
async def test_cas_store_task_succeeds_on_matching_version(tmp_path):
    backend = SQLiteBackend(tmp_path / "db.sqlite")
    await backend.initialize()
    rec = TaskRecord(title="t", version=0)
    await backend.store_task(rec.id, rec.to_dict())

    rec.version = 1
    rec.owner_id = "inst-1"
    ok = await backend.cas_store_task(rec.id, rec.to_dict(), expected_version=0)
    assert ok is True
    reloaded = await backend.load_task(rec.id)
    assert reloaded["version"] == 1
    assert reloaded["owner_id"] == "inst-1"
    await backend.close()


@pytest.mark.asyncio
async def test_cas_store_task_fails_on_stale_version(tmp_path):
    backend = SQLiteBackend(tmp_path / "db.sqlite")
    await backend.initialize()
    rec = TaskRecord(title="t", version=0)
    await backend.store_task(rec.id, rec.to_dict())
    # First writer wins: bumps to version 1.
    await backend.cas_store_task(rec.id, {**rec.to_dict(), "version": 1}, expected_version=0)
    # Second writer holds a stale expected_version=0 → must lose.
    lost = await backend.cas_store_task(rec.id, {**rec.to_dict(), "version": 1}, expected_version=0)
    assert lost is False
    # Losing swap must leave the row untouched (version still at the winner's 1).
    reloaded = await backend.load_task(rec.id)
    assert reloaded["version"] == 1
    await backend.close()


@pytest.mark.asyncio
async def test_cas_store_task_is_version_authoritative_without_caller_prebump(tmp_path):
    """B3 hazard: a read-modify-write caller reads data (version=0), mutates
    status, and calls cas_store_task WITHOUT pre-bumping data["version"]. The
    primitive must still make the stored JSON agree with the column (both →1),
    so the next retry using load_task(...)["version"] as expected_version wins."""
    backend = SQLiteBackend(tmp_path / "db.sqlite")
    await backend.initialize()
    rec = TaskRecord(title="t", version=0)
    await backend.store_task(rec.id, rec.to_dict())

    # Caller did NOT pre-bump: data["version"] is still 0.
    data = await backend.load_task(rec.id)
    assert data["version"] == 0
    data["status"] = "running"
    ok = await backend.cas_store_task(rec.id, data, expected_version=0)
    assert ok is True

    # Column and JSON must agree at 1 despite the caller passing version=0.
    reloaded = await backend.load_task(rec.id)
    assert reloaded["version"] == 1
    assert reloaded["status"] == "running"

    # A subsequent CAS keyed on the reloaded JSON version must succeed, proving
    # column and JSON did not diverge.
    reloaded["status"] = "completed"
    again = await backend.cas_store_task(rec.id, reloaded, expected_version=reloaded["version"])
    assert again is True
    assert (await backend.load_task(rec.id))["version"] == 2
    await backend.close()


@pytest.mark.asyncio
async def test_transition_bumps_version_via_cas(tmp_path):
    backend = SQLiteBackend(tmp_path / "db.sqlite")
    await backend.initialize()
    manager = TaskManager(backend)
    task = await manager.create(title="t")
    await manager.transition(task.id, TaskStatus.QUEUED)

    reloaded = await manager.get(task.id)
    assert reloaded.status == TaskStatus.QUEUED
    assert reloaded.version >= 1  # CAS bumped it
    await backend.close()


@pytest.mark.asyncio
async def test_concurrent_terminal_transitions_do_not_both_win(tmp_path):
    """CAS 保证并发 cancel + fail 从同一 RUNNING 起点竞争两个不同终态时,
    有且只有一方胜出改写终态,另一方被拒绝,终态不会丢失。

    仅断言 final.status ∈ {CANCELLED, FAILED} 无法区分正确的 CAS(恰好一方
    胜出、另一方抛错)与被 last-write-wins 破坏的实现(两方都落盘、后写覆盖
    先写),因为两种情形下最终状态都落在这两个终态之一。这里改为直接捕获两个
    并发协程的结果,断言恰好一个 TaskRecord(胜者)、一个异常(败者),并核对
    持久化终态等于胜者的状态,才能真正守护"终态唯一、无丢失"这一不变式。"""
    backend = SQLiteBackend(tmp_path / "db.sqlite")
    await backend.initialize()
    manager = TaskManager(backend)
    task = await manager.create(title="t")
    await manager.transition(task.id, TaskStatus.QUEUED)
    await manager.transition(task.id, TaskStatus.RUNNING)

    # Two concurrent transitions racing from the shared RUNNING start toward two
    # DIFFERENT terminal states. Do NOT swallow — let exceptions surface so we can
    # prove exactly one won and the other was rejected.
    results = await asyncio.gather(
        manager.transition(task.id, TaskStatus.CANCELLED),
        manager.transition(task.id, TaskStatus.FAILED, error="x"),
        return_exceptions=True,
    )

    # Exactly one coroutine must win (returns a TaskRecord); the other must be
    # rejected. The loser either re-reads a now-terminal status whose allowed-set
    # excludes its target (ValueError) or exhausts CAS retries (TaskCASConflict).
    records = [r for r in results if isinstance(r, TaskRecord)]
    errors = [r for r in results if isinstance(r, Exception)]
    assert len(records) == 1, f"exactly one winner expected, got {results!r}"
    assert len(errors) == 1, f"exactly one loser expected, got {results!r}"
    assert isinstance(errors[0], (ValueError, TaskCASConflict)), repr(errors[0])

    winner = records[0]
    assert winner.status in (TaskStatus.CANCELLED, TaskStatus.FAILED)

    # The persisted terminal status must equal the WINNER's — no lost update where
    # a later write silently overwrites the terminal state the winner committed.
    final = await manager.get(task.id)
    assert final.status == winner.status
    assert final.status in (TaskStatus.CANCELLED, TaskStatus.FAILED)
    await backend.close()


@pytest.mark.asyncio
async def test_reclaim_requeues_expired_and_foreign_running(tmp_path):
    backend = SQLiteBackend(tmp_path / "db.sqlite")
    await backend.initialize()
    manager = TaskManager(backend)

    # 孤儿 A:上一个实例(旧 owner)遗留的 RUNNING。
    a = await manager.create(title="orphan-old-owner")
    await manager.transition(a.id, TaskStatus.QUEUED)
    await manager.transition(a.id, TaskStatus.RUNNING)
    await manager.set_running_context(a.id, "task:a", "evt_a", owner_id="inst-OLD", lease_ttl_ms=60000)

    # 孤儿 B:owner 是自己但租约已过期。
    b = await manager.create(title="orphan-expired")
    await manager.transition(b.id, TaskStatus.QUEUED)
    await manager.transition(b.id, TaskStatus.RUNNING)
    await manager.set_running_context(b.id, "task:b", "evt_b", owner_id="inst-NEW", lease_ttl_ms=-1)

    # 健康 C:owner 自己且租约有效,不该被回收。
    c = await manager.create(title="healthy")
    await manager.transition(c.id, TaskStatus.QUEUED)
    await manager.transition(c.id, TaskStatus.RUNNING)
    await manager.set_running_context(c.id, "task:c", "evt_c", owner_id="inst-NEW", lease_ttl_ms=60000)

    reclaimed = await manager.reclaim_expired_running(current_owner_id="inst-NEW", now_ms=_now_ms())

    assert set(reclaimed) == {a.id, b.id}
    assert (await manager.get(a.id)).status == TaskStatus.QUEUED
    assert (await manager.get(b.id)).status == TaskStatus.QUEUED
    assert (await manager.get(c.id)).status == TaskStatus.RUNNING
    await backend.close()


@pytest.mark.asyncio
async def test_renew_lease_only_for_matching_owner(tmp_path):
    backend = SQLiteBackend(tmp_path / "db.sqlite")
    await backend.initialize()
    manager = TaskManager(backend)
    t = await manager.create(title="t")
    await manager.transition(t.id, TaskStatus.QUEUED)
    await manager.transition(t.id, TaskStatus.RUNNING)
    await manager.set_running_context(t.id, "task:t", "evt", owner_id="inst-1", lease_ttl_ms=1000)

    before = (await manager.get(t.id)).lease_until_ms
    assert await manager.renew_lease(t.id, owner_id="inst-1", lease_ttl_ms=60000) is True
    assert (await manager.get(t.id)).lease_until_ms > before
    assert await manager.renew_lease(t.id, owner_id="other", lease_ttl_ms=60000) is False
    await backend.close()


@pytest.mark.asyncio
async def test_terminal_listener_fires_on_terminal(tmp_path):
    backend = SQLiteBackend(tmp_path / "db.sqlite")
    await backend.initialize()
    manager = TaskManager(backend)
    seen: list[tuple[str, str]] = []

    async def listener(task_id, status):
        seen.append((task_id, status.value))

    manager.add_terminal_listener(listener)
    t = await manager.create(title="t")
    await manager.transition(t.id, TaskStatus.QUEUED)
    await manager.transition(t.id, TaskStatus.RUNNING)  # not terminal → no fire
    await manager.transition(t.id, TaskStatus.CANCELLED)  # terminal → fire

    assert seen == [(t.id, "cancelled")]
    await backend.close()
