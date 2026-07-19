"""引用即强化：搜索不再 touch，reinforce 才是强化信号。"""
from pathlib import Path

import pytest

from echo_agent.memory.store import MemoryStore
from echo_agent.memory.types import MemoryEntry, MemoryType


@pytest.fixture
def store(tmp_path: Path) -> MemoryStore:
    return MemoryStore(tmp_path / "memory")


def _add(store, key="k", content="记住我喜欢Python"):
    return store.add(MemoryEntry(type=MemoryType.USER, key=key, content=content))


class TestSearchNoLongerTouches:
    def test_search_scored_does_not_touch(self, store):
        e = _add(store)
        store.search_scored("Python")
        assert store.get(e.id).access_count == 0
        assert store.get(e.id).last_accessed == ""

    def test_search_keyword_does_not_touch(self, store):
        e = _add(store)
        store.search_keyword("Python")
        assert store.get(e.id).access_count == 0


class TestReinforce:
    def test_reinforce_touches_and_marks_dirty(self, store):
        e = _add(store)
        count = store.reinforce([e.id])
        assert count == 1
        assert store.get(e.id).access_count == 1
        assert store.get(e.id).last_accessed != ""
        assert e.id in store._dirty_ids

    def test_reinforce_skips_unknown_ids(self, store):
        e = _add(store)
        assert store.reinforce(["nonexistent", e.id]) == 1

    def test_reinforce_empty_is_zero(self, store):
        assert store.reinforce([]) == 0


class TestMemoryToolSearchReinforces:
    @pytest.mark.asyncio
    async def test_tool_search_reinforces_results(self, store):
        from echo_agent.agent.tools.memory import MemoryTool

        from echo_agent.memory.service import MemoryService

        e = _add(store)
        tool = MemoryTool(MemoryService(store))
        result = await tool.execute({"action": "search", "target": "user", "query": "Python"})
        assert result.success
        assert store.get(e.id).access_count == 1  # 工具搜索=结果进入上下文=强化一次
