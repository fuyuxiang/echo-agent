from echo_agent.memory.store import MemoryStore
from echo_agent.memory.eligibility import Audience
from echo_agent.memory.types import MemoryEntry, MemoryType


def _store(tmp_path):
    return MemoryStore(memory_dir=tmp_path / "mem")


def test_superseded_hidden_from_tool_search(tmp_path):
    s = _store(tmp_path)
    live = s.add(MemoryEntry(type=MemoryType.USER, key="city", content="上海", source="user_stated"))
    old = s.add(MemoryEntry(type=MemoryType.USER, key="city_old", content="北京住址", source="user_stated"))
    s.mark_superseded(old.id, live.id)
    hits = s.search_scored("北京", session_key=None, audience=Audience.TOOL)
    assert all(e.id != old.id for e, _ in hits)
    # ADMIN 仍可见
    hits_admin = s.search_scored("北京", session_key=None, audience=Audience.ADMIN)
    assert any(e.id == old.id for e, _ in hits_admin)


def test_audience_none_keeps_superseded_for_write_path(tmp_path):
    s = _store(tmp_path)
    live = s.add(MemoryEntry(type=MemoryType.USER, key="a", content="new", source="user_stated"))
    old = s.add(MemoryEntry(type=MemoryType.USER, key="a_old", content="old", source="user_stated"))
    s.mark_superseded(old.id, live.id)
    # find_by_key 默认 audience=None，写入路径须能定位到 superseded 条目
    assert s.find_by_key("a_old") is not None
