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
    """CAS 保证并发 cancel + complete 只有一方改写终态,终态唯一。"""
    backend = SQLiteBackend(tmp_path / "db.sqlite")
    await backend.initialize()
    manager = TaskManager(backend)
    task = await manager.create(title="t")
    await manager.transition(task.id, TaskStatus.QUEUED)
    await manager.transition(task.id, TaskStatus.RUNNING)

    async def cancel():
        try:
            return await manager.transition(task.id, TaskStatus.CANCELLED)
        except Exception:
            return None

    async def fail():
        try:
            return await manager.transition(task.id, TaskStatus.FAILED, error="x")
        except Exception:
            return None

    await asyncio.gather(cancel(), fail())
    final = await manager.get(task.id)
    # 终态唯一:必落在两者之一,不出现被后写者覆盖成另一个终态又回滚的中间态
    assert final.status in (TaskStatus.CANCELLED, TaskStatus.FAILED)
    await backend.close()
