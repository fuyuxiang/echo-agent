# tests/test_unresolved_tracker.py
from pathlib import Path

from echo_agent.memory.store import MemoryStore


def _store(tmp_path: Path) -> MemoryStore:
    return MemoryStore(memory_dir=tmp_path / "mem")


def test_mark_and_query_unresolved(tmp_path):
    store = _store(tmp_path)
    assert store.is_unresolved("m1") is False
    store.mark_contradiction_unresolved("c1", "m1", "m2")
    assert store.is_unresolved("m1") is True
    assert store.is_unresolved("m2") is True
    assert store.is_unresolved("m3") is False


def test_clear_removes_only_when_refcount_zero(tmp_path):
    store = _store(tmp_path)
    # m2 涉及两个未解决矛盾
    store.mark_contradiction_unresolved("c1", "m1", "m2")
    store.mark_contradiction_unresolved("c2", "m2", "m3")
    store.clear_contradiction("c1")
    # m1 只在 c1 中 -> 解除;m2 仍在 c2 -> 保留
    assert store.is_unresolved("m1") is False
    assert store.is_unresolved("m2") is True
    assert store.is_unresolved("m3") is True
    store.clear_contradiction("c2")
    assert store.is_unresolved("m2") is False
    assert store.is_unresolved("m3") is False


def test_mark_is_idempotent(tmp_path):
    store = _store(tmp_path)
    store.mark_contradiction_unresolved("c1", "m1", "m2")
    store.mark_contradiction_unresolved("c1", "m1", "m2")  # 重复
    store.clear_contradiction("c1")
    assert store.is_unresolved("m1") is False  # 不应残留计数


def test_clear_unknown_id_is_noop(tmp_path):
    store = _store(tmp_path)
    store.clear_contradiction("nope")  # 不抛异常
    assert store.is_unresolved("m1") is False


def test_reset_clears_all(tmp_path):
    store = _store(tmp_path)
    store.mark_contradiction_unresolved("c1", "m1", "m2")
    store.reset_unresolved()
    assert store.is_unresolved("m1") is False
