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
