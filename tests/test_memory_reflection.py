"""反思引擎：归纳提炼（distill）。"""
import json
from pathlib import Path
from unittest.mock import AsyncMock, MagicMock

import pytest

from echo_agent.memory.reflection import ReflectionEngine
from echo_agent.memory.store import MemoryStore
from echo_agent.memory.types import MemoryEntry, MemoryType


@pytest.fixture
def store(tmp_path: Path) -> MemoryStore:
    return MemoryStore(tmp_path / "memory")


def _llm_returning(tool_name: str, args: dict):
    resp = MagicMock()
    tc = MagicMock()
    tc.name = tool_name
    tc.arguments = json.dumps(args)
    resp.tool_calls = [tc]
    return AsyncMock(return_value=resp)


def _add_prefixed(store, n, prefix="pref"):
    entries = []
    for i in range(n):
        entries.append(store.add(MemoryEntry(
            type=MemoryType.USER, key=f"{prefix}:item{i}",
            content=f"喜欢事物{i}", source="model_inferred",
        )))
    return entries


class TestDistill:
    @pytest.mark.asyncio
    async def test_distills_group_of_three(self, store):
        _add_prefixed(store, 3)
        llm = _llm_returning("save_distilled", {
            "distill": True, "key": "pref:general",
            "content": "用户对新事物普遍持开放喜好", "importance": 0.7,
        })
        engine = ReflectionEngine(store, llm_call=llm)
        created = await engine.distill()
        assert created == 1
        general = store.find_by_key("pref:general")
        assert general is not None
        assert general.source == "consolidated"
        assert "distilled" in general.tags
        # 只增不删：原条目全在
        assert store.find_by_key("pref:item0") is not None

    @pytest.mark.asyncio
    async def test_skips_group_below_threshold(self, store):
        _add_prefixed(store, 2)  # 少于 3 条
        llm = AsyncMock()
        engine = ReflectionEngine(store, llm_call=llm)
        assert await engine.distill() == 0
        llm.assert_not_awaited()

    @pytest.mark.asyncio
    async def test_skips_group_with_existing_general(self, store):
        _add_prefixed(store, 3)
        store.add(MemoryEntry(
            type=MemoryType.USER, key="pref:general",
            content="已有规律", source="consolidated",
        ))
        llm = AsyncMock()
        engine = ReflectionEngine(store, llm_call=llm)
        assert await engine.distill() == 0
        llm.assert_not_awaited()

    @pytest.mark.asyncio
    async def test_llm_declines_distill(self, store):
        _add_prefixed(store, 3)
        llm = _llm_returning("save_distilled", {"distill": False})
        engine = ReflectionEngine(store, llm_call=llm)
        assert await engine.distill() == 0

    @pytest.mark.asyncio
    async def test_max_groups_cap(self, store):
        _add_prefixed(store, 3, prefix="a")
        _add_prefixed(store, 3, prefix="b")
        _add_prefixed(store, 3, prefix="c")
        llm = _llm_returning("save_distilled", {"distill": False})
        engine = ReflectionEngine(store, llm_call=llm)
        await engine.distill(max_groups=2)
        assert llm.await_count == 2

    @pytest.mark.asyncio
    async def test_llm_failure_returns_zero(self, store):
        _add_prefixed(store, 3)
        engine = ReflectionEngine(store, llm_call=AsyncMock(side_effect=RuntimeError("boom")))
        assert await engine.distill() == 0  # 不抛异常


def test_config_reflection_enabled_default_true():
    from echo_agent.config.schema import MemoryConfig
    assert MemoryConfig().reflection_enabled is True


class TestResolveConflicts:
    def _pair(self, store, src_a="user_stated", src_b="model_inferred"):
        from echo_agent.memory.store import MemoryStore as _MS
        a = store.add(MemoryEntry(
            type=MemoryType.USER, key="home:city", content="住北京",
            source=src_a, tags=[_MS.SUSPECTED_CONFLICT_TAG],
        ))
        b = MemoryEntry(
            type=MemoryType.USER, key="home:addr", content="搬到了上海",
            source=src_b, tags=[_MS.SUSPECTED_CONFLICT_TAG],
        )
        store._entries[b.id] = b  # 绕过守卫构造并存冲突对
        return a, b

    def _engine(self, store, verdict_args, detector=None):
        llm = _llm_returning("adjudicate", verdict_args)
        if detector is None:
            detector = MagicMock()
            detector.get_unresolved = AsyncMock(return_value=[])
            detector.resolve = AsyncMock()
            detector.store_contradiction = AsyncMock()
        return ReflectionEngine(store, llm_call=llm, contradiction_detector=detector), detector

    @pytest.mark.asyncio
    async def test_clear_verdict_supersedes(self, store):
        a, b = self._pair(store)
        engine, detector = self._engine(store, {"verdict": "b_wins", "explanation": "时效替代"})
        stats = await engine.resolve_conflicts()
        assert stats["resolved"] == 1
        assert store.get(a.id).is_superseded
        # 双方 suspected_conflict 已清
        from echo_agent.memory.store import MemoryStore as _MS
        assert _MS.SUSPECTED_CONFLICT_TAG not in store.get(a.id).tags
        assert _MS.SUSPECTED_CONFLICT_TAG not in store.get(b.id).tags

    @pytest.mark.asyncio
    async def test_ambiguous_defers_to_user(self, store):
        a, b = self._pair(store)
        engine, _ = self._engine(store, {"verdict": "ambiguous", "explanation": "无法判断"})
        stats = await engine.resolve_conflicts()
        assert stats["deferred"] == 1
        assert ReflectionEngine.NEEDS_CONFIRMATION_TAG in store.get(a.id).tags
        assert ReflectionEngine.NEEDS_CONFIRMATION_TAG in store.get(b.id).tags
        assert not store.get(a.id).is_superseded

    @pytest.mark.asyncio
    async def test_not_contradictory_clears_tags(self, store):
        a, b = self._pair(store)
        engine, _ = self._engine(store, {"verdict": "not_contradictory", "explanation": "不矛盾"})
        stats = await engine.resolve_conflicts()
        assert stats["dismissed"] == 1
        from echo_agent.memory.store import MemoryStore as _MS
        assert _MS.SUSPECTED_CONFLICT_TAG not in store.get(a.id).tags
        assert not store.get(a.id).is_superseded

    @pytest.mark.asyncio
    async def test_invalid_verdict_treated_ambiguous(self, store):
        """白名单：幻觉输出按模糊处理。"""
        a, b = self._pair(store)
        engine, _ = self._engine(store, {"verdict": "hallucinated_option"})
        stats = await engine.resolve_conflicts()
        assert stats["deferred"] == 1
        assert not store.get(a.id).is_superseded

    @pytest.mark.asyncio
    async def test_max_pairs_cap(self, store):
        from echo_agent.memory.store import MemoryStore as _MS
        for i in range(5):
            e = MemoryEntry(
                type=MemoryType.USER, key=f"g{i}:x", content=f"甲{i}",
                source="model_inferred", tags=[_MS.SUSPECTED_CONFLICT_TAG],
            )
            e2 = MemoryEntry(
                type=MemoryType.USER, key=f"g{i}:y", content=f"乙{i}",
                source="model_inferred", tags=[_MS.SUSPECTED_CONFLICT_TAG],
            )
            store._entries[e.id] = e
            store._entries[e2.id] = e2
        llm = _llm_returning("adjudicate", {"verdict": "not_contradictory"})
        detector = MagicMock()
        detector.get_unresolved = AsyncMock(return_value=[])
        detector.resolve = AsyncMock()
        engine = ReflectionEngine(store, llm_call=llm, contradiction_detector=detector)
        await engine.resolve_conflicts(max_pairs=3)
        assert llm.await_count == 3


class TestSnapshotConfirmationNotice:
    def test_snapshot_appends_notice_when_pending(self, store):
        e = store.add(MemoryEntry(
            type=MemoryType.USER, key="k", content="待确认内容", importance=0.9,
        ))
        store.update(e.id, tags=[ReflectionEngine.NEEDS_CONFIRMATION_TAG])
        snapshot, _ = store.get_snapshot_with_ids()
        assert "need your confirmation" in snapshot

    def test_snapshot_no_notice_when_none(self, store):
        store.add(MemoryEntry(type=MemoryType.USER, key="k", content="普通内容"))
        snapshot, _ = store.get_snapshot_with_ids()
        assert "need your confirmation" not in snapshot


class TestConsolidatorStep6:
    @pytest.mark.asyncio
    async def test_sleep_consolidate_runs_reflection(self, store):
        from echo_agent.memory.consolidator import MemoryConsolidator

        c = MemoryConsolidator(store, llm_call=AsyncMock())
        engine = MagicMock()
        engine.run = AsyncMock(return_value={"distilled": 1, "resolved": 0})
        c.set_reflection(engine)
        stats = await c.sleep_consolidate("s1", [])
        engine.run.assert_awaited_once()
        assert stats.get("distilled") == 1

    @pytest.mark.asyncio
    async def test_reflection_failure_does_not_break_consolidation(self, store):
        from echo_agent.memory.consolidator import MemoryConsolidator

        c = MemoryConsolidator(store, llm_call=AsyncMock())
        engine = MagicMock()
        engine.run = AsyncMock(side_effect=RuntimeError("reflection boom"))
        c.set_reflection(engine)
        stats = await c.sleep_consolidate("s1", [])  # 不抛异常
        assert "episodes" in stats
