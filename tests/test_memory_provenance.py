"""记忆来源分级（provenance）：字段序列化、优先级函数、打标、裁决。"""
import pytest

from pathlib import Path

from echo_agent.agent.tools.memory import MemoryTool
from echo_agent.memory.store import MemoryStore
from echo_agent.memory.types import MemoryEntry, source_priority


@pytest.fixture
def store(tmp_path: Path) -> MemoryStore:
    return MemoryStore(tmp_path / "memory")


class TestSourceField:
    def test_default_is_legacy(self):
        assert MemoryEntry().source == "legacy"

    def test_serialization_roundtrip(self):
        e = MemoryEntry(key="k", content="c", source="user_stated")
        restored = MemoryEntry.from_dict(e.to_dict())
        assert restored.source == "user_stated"

    def test_from_dict_missing_source_falls_to_legacy(self):
        """存量 JSON 无 source 字段 → legacy。"""
        e = MemoryEntry(key="k", content="c")
        data = e.to_dict()
        del data["source"]
        assert MemoryEntry.from_dict(data).source == "legacy"


class TestSourcePriority:
    def test_ordering(self):
        assert (
            source_priority("user_stated")
            > source_priority("consolidated")
            > source_priority("model_inferred")
            > source_priority("legacy")
        )

    def test_exact_values(self):
        assert source_priority("user_stated") == 3
        assert source_priority("consolidated") == 2
        assert source_priority("model_inferred") == 1
        assert source_priority("legacy") == 0

    def test_unknown_word_is_zero(self):
        assert source_priority("some_future_source") == 0
        assert source_priority("") == 0


class TestWritePathStamping:
    @pytest.mark.asyncio
    async def test_memory_tool_add_defaults_model_inferred(self, store):
        tool = MemoryTool(store)
        await tool.execute({"action": "add", "target": "user", "key": "k1", "content": "喜欢Python"})
        assert store.find_by_key("k1").source == "model_inferred"

    @pytest.mark.asyncio
    async def test_memory_tool_add_explicit_user_stated(self, store):
        tool = MemoryTool(store)
        await tool.execute({
            "action": "add", "target": "user", "key": "k2",
            "content": "用户明确说住在北京", "source": "user_stated",
        })
        assert store.find_by_key("k2").source == "user_stated"

    @pytest.mark.asyncio
    async def test_memory_tool_add_rejects_unknown_source_to_default(self, store):
        """schema 之外的词不采纳，落默认 model_inferred。"""
        tool = MemoryTool(store)
        await tool.execute({
            "action": "add", "target": "user", "key": "k3",
            "content": "c", "source": "hacker_injected",
        })
        assert store.find_by_key("k3").source == "model_inferred"

    @pytest.mark.asyncio
    async def test_memory_tool_replace_updates_source(self, store):
        tool = MemoryTool(store)
        await tool.execute({
            "action": "add", "target": "user", "key": "k4",
            "content": "旧内容", "source": "user_stated",
        })
        await tool.execute({
            "action": "replace", "target": "user", "key": "k4", "content": "新内容",
        })
        e = store.find_by_key("k4")
        assert e.content == "新内容"
        assert e.source == "model_inferred"  # replace 未声明 → 保守默认

    def test_reviewer_stamps_model_inferred(self, store):
        from echo_agent.memory.reviewer import MemoryReviewer
        from unittest.mock import MagicMock

        reviewer = MemoryReviewer(provider=MagicMock(), store=store)
        result = reviewer._execute({
            "action": "add", "target": "user", "key": "k5", "content": "推断的偏好",
        })
        assert result.startswith("Added")
        assert store.find_by_key("k5").source == "model_inferred"

    @pytest.mark.asyncio
    async def test_promote_stamps_consolidated(self, store):
        from echo_agent.memory.tiers import SemanticManager
        from echo_agent.memory.types import Episode

        mgr = SemanticManager(store)
        episode = Episode(session_key="s1", summary="聊了项目部署")
        promoted = await mgr.promote_from_episodic(
            episode, [{"key": "k6", "content": "项目用docker部署", "type": "environment"}],
        )
        assert promoted[0].source == "consolidated"

    def test_store_update_source_none_keeps(self, store):
        from echo_agent.memory.types import MemoryEntry, MemoryType

        e = store.add(MemoryEntry(type=MemoryType.USER, key="k7", content="c", source="user_stated"))
        store.update(e.id, content="c2")
        assert store.get(e.id).source == "user_stated"
        store.update(e.id, content="c3", source="model_inferred")
        assert store.get(e.id).source == "model_inferred"
