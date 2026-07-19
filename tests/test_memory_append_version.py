import pytest

from echo_agent.memory.store import MemoryStore
from echo_agent.memory.types import MemoryEntry, MemoryType


def _store(tmp_path):
    return MemoryStore(memory_dir=tmp_path / "mem", scope_policy="session")


def test_append_version_preserves_old_and_bumps(tmp_path):
    s = _store(tmp_path)
    old = s.add(MemoryEntry(type=MemoryType.USER, key="home", content="北京",
                            source="user_stated", source_session="x"))
    new = MemoryEntry(type=MemoryType.USER, key="home", content="上海",
                      source="user_stated", source_session="x")
    result = s.append_version(old.id, new)
    assert result.version == old.version + 1        # 新版本 version+1
    assert s.get(old.id) is not None                # 旧版本保留
    assert s.get(old.id).superseded_by == result.id # 旧指向新
    assert not s.get(result.id).is_superseded        # 新是 active


def test_add_same_key_higher_or_equal_appends(tmp_path):
    s = _store(tmp_path)
    s.add(MemoryEntry(type=MemoryType.USER, key="home", content="北京",
                      source="user_stated", source_session="x"))
    r = s.add(MemoryEntry(type=MemoryType.USER, key="home", content="上海",
                          source="user_stated", source_session="x"))
    assert r.content == "上海" and r.version == 2       # 走 append 非覆盖
    all_home = [e for e in s._entries.values() if e.key == "home"]
    assert len(all_home) == 2                            # 旧版本保留(未被覆盖)
    assert any(e.content == "北京" and e.is_superseded for e in all_home)


def test_add_same_key_lower_priority_keeps_old_no_overwrite(tmp_path):
    s = _store(tmp_path)
    s.add(MemoryEntry(type=MemoryType.USER, key="home", content="上海",
                      source="user_stated", source_session="x"))
    r = s.add(MemoryEntry(type=MemoryType.USER, key="home", content="北京",
                          source="model_inferred", source_session="x"))
    assert r.content == "上海"                           # 低优先级不覆盖,保留旧
    live = [e for e in s._entries.values() if e.key == "home" and not e.is_superseded]
    assert len(live) == 1 and live[0].content == "上海"  # 无新 active 版本


def test_conflict_ignores_superseded(tmp_path):
    s = _store(tmp_path)
    s.add(MemoryEntry(type=MemoryType.USER, key="home", content="北京",
                      source="user_stated", source_session="x"))
    s.add(MemoryEntry(type=MemoryType.USER, key="home", content="上海",
                      source="user_stated", source_session="x"))  # 北京→superseded
    # 第三次改口应以 active(上海)为冲突基准,不受 superseded(北京)干扰
    r = s.add(MemoryEntry(type=MemoryType.USER, key="home", content="广州",
                          source="user_stated", source_session="x"))
    live = [e for e in s._entries.values() if e.key == "home" and not e.is_superseded]
    assert len(live) == 1 and live[0].content == "广州" and live[0].version == 3


def test_capacity_counts_only_active(tmp_path):
    s = MemoryStore(memory_dir=tmp_path / "mem", scope_policy="session", max_user=2)
    s.add(MemoryEntry(type=MemoryType.USER, key="a", content="v1", source="user_stated", source_session="x"))
    s.add(MemoryEntry(type=MemoryType.USER, key="a", content="v2", source="user_stated", source_session="x"))  # a: 1 active + 1 superseded
    s.add(MemoryEntry(type=MemoryType.USER, key="b", content="w", source="user_stated", source_session="x"))
    # active 计数为 2(a-v2, b),未超 max_user=2,b 不应触发淘汰 a
    live = [e for e in s._entries.values() if not e.is_superseded]
    assert len([e for e in live if e.key == "b"]) == 1
