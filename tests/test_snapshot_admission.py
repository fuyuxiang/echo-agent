# tests/test_snapshot_admission.py
from pathlib import Path

from echo_agent.memory.store import MemoryStore
from echo_agent.memory.types import MemoryEntry, MemoryType


def _store(tmp_path: Path) -> MemoryStore:
    return MemoryStore(memory_dir=tmp_path / "mem")


def _add(store, *, mid, content, mtype=MemoryType.USER, importance=0.8, access=3):
    e = MemoryEntry(id=mid, type=mtype, key=mid, content=content, importance=importance)
    e.touch()  # sets last_accessed so decay is well-defined
    e.access_count = access  # set after touch(): touch() increments access_count in this codebase
    store.add(e)
    return e


def test_unresolved_pair_excluded_then_readmitted(tmp_path):
    store = _store(tmp_path)
    _add(store, mid="m1", content="likes tea")
    _add(store, mid="m2", content="hates tea")
    store.mark_contradiction_unresolved("c1", "m1", "m2")
    _text, ids = store.get_snapshot_with_ids()
    assert "m1" not in ids and "m2" not in ids
    # resolve -> readmitted
    store.clear_contradiction("c1")
    _text2, ids2 = store.get_snapshot_with_ids()
    assert "m1" in ids2 and "m2" in ids2


def test_low_confidence_user_fact_excluded(tmp_path):
    store = _store(tmp_path)
    _add(store, mid="hi", content="confirmed name", importance=0.9, access=5)
    _add(store, mid="lo", content="maybe likes jazz", importance=0.2, access=0)
    _text, ids = store.get_snapshot_with_ids()
    assert "hi" in ids
    assert "lo" not in ids


def test_environment_low_access_still_admitted(tmp_path):
    store = _store(tmp_path)
    _add(store, mid="env1", content="project uses pytest",
         mtype=MemoryType.ENVIRONMENT, importance=0.2, access=0)
    _text, ids = store.get_snapshot_with_ids()
    assert "env1" in ids  # env not subject to confidence soft gate


def test_get_snapshot_returns_text_only_and_matches(tmp_path):
    store = _store(tmp_path)
    _add(store, mid="m1", content="likes tea")
    text = store.get_snapshot()
    text2, ids = store.get_snapshot_with_ids()
    assert text == text2
    assert "m1" in ids
