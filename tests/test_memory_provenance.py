"""记忆来源分级（provenance）：字段序列化、优先级函数、打标、裁决。"""
import pytest

from pathlib import Path

from echo_agent.agent.tools.memory import MemoryTool
from echo_agent.memory.service import MemoryService
from echo_agent.memory.store import MemoryStore
from echo_agent.memory.types import MemoryEntry, source_priority
from echo_agent.tools.base import ToolExecutionContext


@pytest.fixture
def store(tmp_path: Path) -> MemoryStore:
    return MemoryStore(tmp_path / "memory")


# 工具已改走 service;写操作须经 execute 并带一个含 memory_scope 的 ctx
# (USER 写的 scope 门禁在 service),用它取代此前的裸 MemoryTool(store)。
_CTX = ToolExecutionContext(session_key="s", memory_scope="scope1")


def _tool(store: MemoryStore) -> MemoryTool:
    return MemoryTool(service=MemoryService(store))


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
        tool = _tool(store)
        await tool.execute({"action": "add", "target": "user", "key": "k1", "content": "喜欢Python"}, _CTX)
        assert store.find_by_key("k1").source == "model_inferred"

    @pytest.mark.asyncio
    async def test_memory_tool_add_explicit_user_stated(self, store):
        tool = _tool(store)
        await tool.execute({
            "action": "add", "target": "user", "key": "k2",
            "content": "用户明确说住在北京", "source": "user_stated",
        }, _CTX)
        assert store.find_by_key("k2").source == "user_stated"

    @pytest.mark.asyncio
    async def test_memory_tool_add_rejects_unknown_source_to_default(self, store):
        """schema 之外的词不采纳，落默认 model_inferred。"""
        tool = _tool(store)
        await tool.execute({
            "action": "add", "target": "user", "key": "k3",
            "content": "c", "source": "hacker_injected",
        }, _CTX)
        assert store.find_by_key("k3").source == "model_inferred"

    @pytest.mark.asyncio
    async def test_memory_tool_replace_updates_source(self, store):
        # 原始条目改为 model_inferred:replace 默认来源也是 model_inferred,等优先级
        # 可覆盖,验证"未声明 source → 保守默认 model_inferred"这一原意。若基础条目为
        # user_stated,provenance_guard 会拦截 model_inferred 覆盖(见
        # test_provenance_bypass_regression),那是有意的新语义,非本用例所测。
        tool = _tool(store)
        await tool.execute({
            "action": "add", "target": "user", "key": "k4",
            "content": "旧内容", "source": "model_inferred",
        }, _CTX)
        await tool.execute({
            "action": "replace", "target": "user", "key": "k4", "content": "新内容",
        }, _CTX)
        e = store.find_by_key("k4")
        assert e.content == "新内容"
        assert e.source == "model_inferred"  # replace 未声明 → 保守默认

    @pytest.mark.asyncio
    async def test_reviewer_stamps_model_inferred(self, store):
        from echo_agent.memory.reviewer import MemoryReviewer
        from unittest.mock import MagicMock

        # USER 写:scope 门禁在 service,须给 reviewer 一个 session_key 作 memory_scope。
        reviewer = MemoryReviewer(provider=MagicMock(), service=MemoryService(store), session_key="s1")
        result = await reviewer._execute({
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


class TestMergeGuard:
    def test_lower_priority_does_not_overwrite(self, store):
        from echo_agent.memory.types import MemoryEntry, MemoryType

        store.add(MemoryEntry(
            type=MemoryType.USER, key="pref:drink", content="喝茶",
            source="user_stated", source_session="s1",
        ))
        merged = store.add(MemoryEntry(
            type=MemoryType.USER, key="pref:drink", content="喝咖啡",
            source="model_inferred", source_session="s1",
        ))
        assert merged.content == "喝茶"  # 用户明说的内容保住了
        assert merged.source == "user_stated"
        assert store.SUSPECTED_CONFLICT_TAG in merged.tags  # 冲突留痕

    def test_equal_priority_overwrites(self, store):
        from echo_agent.memory.types import MemoryEntry, MemoryType

        store.add(MemoryEntry(
            type=MemoryType.USER, key="pref:editor", content="vim",
            source="model_inferred", source_session="s1",
        ))
        merged = store.add(MemoryEntry(
            type=MemoryType.USER, key="pref:editor", content="emacs",
            source="model_inferred", source_session="s1",
        ))
        assert merged.content == "emacs"

    def test_higher_priority_overwrites_and_adopts_source(self, store):
        from echo_agent.memory.types import MemoryEntry, MemoryType

        store.add(MemoryEntry(
            type=MemoryType.USER, key="pref:lang", content="推断喜欢Go",
            source="model_inferred", source_session="s1",
        ))
        merged = store.add(MemoryEntry(
            type=MemoryType.USER, key="pref:lang", content="用户明说喜欢Rust",
            source="user_stated", source_session="s1",
        ))
        assert merged.content == "用户明说喜欢Rust"
        assert merged.source == "user_stated"

    def test_legacy_overwritten_by_any_new_write(self, store):
        """存量 legacy=0 最低，任何新写入都可覆盖——与升级前行为一致。"""
        from echo_agent.memory.types import MemoryEntry, MemoryType

        store.add(MemoryEntry(
            type=MemoryType.USER, key="pref:os", content="旧记录",
            source="legacy", source_session="s1",
        ))
        merged = store.add(MemoryEntry(
            type=MemoryType.USER, key="pref:os", content="新推断",
            source="model_inferred", source_session="s1",
        ))
        assert merged.content == "新推断"


class TestAutoResolvePriority:
    def _consolidator(self, store):
        from echo_agent.memory.consolidator import MemoryConsolidator
        from unittest.mock import AsyncMock, MagicMock

        c = MemoryConsolidator(store, llm_call=AsyncMock())
        detector = MagicMock()
        detector.resolve = AsyncMock()
        c.set_contradiction_detector(detector)
        return c, detector

    @pytest.mark.asyncio
    async def test_higher_priority_wins_even_if_older(self, store):
        from echo_agent.memory.types import Contradiction, MemoryEntry, MemoryType

        old_user = store.add(MemoryEntry(
            type=MemoryType.USER, key="home", content="住北京",
            source="user_stated", source_session="s1",
        ))
        old_user.updated_at = "2026-01-01T00:00:00"
        newer_inferred = MemoryEntry(
            type=MemoryType.USER, key="home", content="住上海",
            source="model_inferred", source_session="s1",
        )
        store._entries[newer_inferred.id] = newer_inferred

        c, detector = self._consolidator(store)
        contradiction = Contradiction(
            memory_id_a=old_user.id, memory_id_b=newer_inferred.id,
        )
        entry_map = {old_user.id: old_user, newer_inferred.id: newer_inferred}
        resolved = await c._auto_resolve_same_key(contradiction, entry_map)
        assert resolved is True
        detector.resolve.assert_awaited_once()
        assert detector.resolve.await_args.kwargs["winner_id"] == old_user.id

    @pytest.mark.asyncio
    async def test_equal_priority_falls_to_newest_wins(self, store):
        from echo_agent.memory.types import Contradiction, MemoryEntry, MemoryType

        older = store.add(MemoryEntry(
            type=MemoryType.USER, key="job", content="工程师",
            source="model_inferred", source_session="s1",
        ))
        older.updated_at = "2026-01-01T00:00:00"
        newer = MemoryEntry(
            type=MemoryType.USER, key="job", content="架构师",
            source="model_inferred", source_session="s1",
        )
        newer.updated_at = "2026-06-01T00:00:00"
        store._entries[newer.id] = newer

        c, detector = self._consolidator(store)
        contradiction = Contradiction(memory_id_a=older.id, memory_id_b=newer.id)
        entry_map = {older.id: older, newer.id: newer}
        assert await c._auto_resolve_same_key(contradiction, entry_map) is True
        assert detector.resolve.await_args.kwargs["winner_id"] == newer.id

    @pytest.mark.asyncio
    async def test_legacy_party_skips_auto_resolve(self, store):
        from echo_agent.memory.types import Contradiction, MemoryEntry, MemoryType

        legacy = store.add(MemoryEntry(
            type=MemoryType.USER, key="city", content="旧数据",
            source="legacy", source_session="s1",
        ))
        newer = MemoryEntry(
            type=MemoryType.USER, key="city", content="新推断",
            source="model_inferred", source_session="s1",
        )
        store._entries[newer.id] = newer

        c, detector = self._consolidator(store)
        contradiction = Contradiction(memory_id_a=legacy.id, memory_id_b=newer.id)
        entry_map = {legacy.id: legacy, newer.id: newer}
        assert await c._auto_resolve_same_key(contradiction, entry_map) is False
        detector.resolve.assert_not_awaited()
