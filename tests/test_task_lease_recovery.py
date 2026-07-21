import pytest

from echo_agent.storage.sqlite import SQLiteBackend
from echo_agent.tasks.models import TaskRecord, TaskCASConflict, _now_ms


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
    await backend.close()
