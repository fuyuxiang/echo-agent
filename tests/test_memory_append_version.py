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
