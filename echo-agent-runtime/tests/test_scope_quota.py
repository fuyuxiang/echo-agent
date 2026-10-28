from __future__ import annotations

from echo_agent.memory.store import MemoryStore
from echo_agent.memory.types import MemoryEntry, MemoryType


def _store(tmp_path):
    # max_user=2:便于触发容量淘汰
    return MemoryStore(memory_dir=tmp_path / "mem", scope_policy="session", max_user=2)


def test_capacity_evicts_within_same_scope_only(tmp_path):
    s = _store(tmp_path)
    # scope A 写 2 条(占满 A 的桶)
    s.add(MemoryEntry(type=MemoryType.USER, key="a:1", content="A-1", source_session="owner"))
    s.add(MemoryEntry(type=MemoryType.USER, key="a:2", content="A-2", source_session="owner"))
    # scope B 写 1 条
    s.add(MemoryEntry(type=MemoryType.USER, key="b:1", content="B-1", source_session="telegram:bob"))
    # A 再写第 3 条应只淘汰 A 自己最旧的,不动 B
    s.add(MemoryEntry(type=MemoryType.USER, key="a:3", content="A-3", source_session="owner"))
    remaining = [e.content for e in s.list_all(mem_type=MemoryType.USER)]
    assert "B-1" in remaining  # B 未被 A 的写入挤掉
    # A 桶内仍是 2 条(A-1 被淘汰,A-2/A-3 在)
    a_contents = [e.content for e in s.list_all(mem_type=MemoryType.USER) if e.source_session == "owner"]
    assert len(a_contents) == 2 and "A-1" not in a_contents
