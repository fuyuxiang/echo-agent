"""反思引擎：归纳提炼（distill）。"""
import json
from pathlib import Path
from unittest.mock import AsyncMock, MagicMock

import pytest

from echo_agent.memory.reflection import ReflectionEngine
from echo_agent.memory.service import MemoryService
from echo_agent.memory.store import MemoryStore
from echo_agent.memory.types import MemoryEntry, MemoryType


@pytest.fixture
def store(tmp_path: Path) -> MemoryStore:
    return MemoryStore(tmp_path / "memory")


def _svc(store) -> MemoryService:
    """reflection 写现走 service 通道,测试用最小 service(无失效/审计)包裹 store。"""
    return MemoryService(store)


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
            content=f"喜欢事物{i}", source="model_inferred", source_session="s1",
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
        engine = ReflectionEngine(_svc(store), llm_call=llm)
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
        engine = ReflectionEngine(_svc(store), llm_call=llm)
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
        engine = ReflectionEngine(_svc(store), llm_call=llm)
        assert await engine.distill() == 0
        llm.assert_not_awaited()

    @pytest.mark.asyncio
    async def test_llm_declines_distill(self, store):
        _add_prefixed(store, 3)
        llm = _llm_returning("save_distilled", {"distill": False})
        engine = ReflectionEngine(_svc(store), llm_call=llm)
        assert await engine.distill() == 0

    @pytest.mark.asyncio
    async def test_max_groups_cap(self, store):
        _add_prefixed(store, 3, prefix="a")
        _add_prefixed(store, 3, prefix="b")
        _add_prefixed(store, 3, prefix="c")
        llm = _llm_returning("save_distilled", {"distill": False})
        engine = ReflectionEngine(_svc(store), llm_call=llm)
        await engine.distill(max_groups=2)
        assert llm.await_count == 2

    @pytest.mark.asyncio
    async def test_llm_failure_returns_zero(self, store):
        _add_prefixed(store, 3)
        engine = ReflectionEngine(_svc(store), llm_call=AsyncMock(side_effect=RuntimeError("boom")))
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
        return ReflectionEngine(_svc(store), llm_call=llm, contradiction_detector=detector), detector

    @pytest.mark.asyncio
    async def test_clear_verdict_supersedes(self, store):
        # b_wins with b=user_stated (pri 3) superseding a=model_inferred (pri 1)
        # — a legitimate supersede that clears the provenance floor.
        a, b = self._pair(store, src_a="model_inferred", src_b="user_stated")
        engine, detector = self._engine(store, {"verdict": "b_wins", "explanation": "时效替代"})
        stats = await engine.resolve_conflicts()
        assert stats["resolved"] == 1
        assert store.get(a.id).is_superseded
        # 双方 suspected_conflict 已清
        from echo_agent.memory.store import MemoryStore as _MS
        assert _MS.SUSPECTED_CONFLICT_TAG not in store.get(a.id).tags
        assert _MS.SUSPECTED_CONFLICT_TAG not in store.get(b.id).tags

    @pytest.mark.asyncio
    async def test_priority_floor_blocks_lower_over_higher(self, store):
        """LLM 判低优先级(model_inferred)顶掉高优先级(user_stated)时降级为待确认，
        不 supersede——与 consolidator 的来源优先级契约对齐。"""
        a, b = self._pair(store, src_a="user_stated", src_b="model_inferred")
        engine, _ = self._engine(store, {"verdict": "b_wins", "explanation": "LLM误判"})
        stats = await engine.resolve_conflicts()
        assert stats["resolved"] == 0
        assert stats["deferred"] == 1
        assert not store.get(a.id).is_superseded
        assert ReflectionEngine.NEEDS_CONFIRMATION_TAG in store.get(a.id).tags

    @pytest.mark.asyncio
    async def test_priority_floor_blocks_legacy(self, store):
        """任一方为 legacy(无来源) 时无裁决依据，降级待确认。"""
        a, b = self._pair(store, src_a="legacy", src_b="user_stated")
        engine, _ = self._engine(store, {"verdict": "b_wins", "explanation": "x"})
        stats = await engine.resolve_conflicts()
        assert stats["resolved"] == 0
        assert stats["deferred"] == 1
        assert not store.get(a.id).is_superseded

    @pytest.mark.asyncio
    async def test_same_priority_lets_llm_stand(self, store):
        """同级来源时 LLM 的内容判断可落地 supersede。"""
        a, b = self._pair(store, src_a="model_inferred", src_b="model_inferred")
        engine, _ = self._engine(store, {"verdict": "a_wins", "explanation": "更准确"})
        stats = await engine.resolve_conflicts()
        assert stats["resolved"] == 1
        assert store.get(b.id).is_superseded

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
        engine = ReflectionEngine(_svc(store), llm_call=llm, contradiction_detector=detector)
        await engine.resolve_conflicts(max_pairs=3)
        assert llm.await_count == 3

    @pytest.mark.asyncio
    async def test_uses_detector_pairing_not_prefix(self, store):
        """检测器给出的权威 (a,b) 配对优先于前缀重建，避免同前缀误配对。"""
        from echo_agent.memory.store import MemoryStore as _MS
        from echo_agent.memory.types import Contradiction
        # 三条同前缀 home:*，前缀重建会取最旧两条(c1,c2)，但真实冲突是 c1 与 c3。
        c1 = store.add(MemoryEntry(
            type=MemoryType.USER, key="home:city", content="住北京",
            source="model_inferred", tags=[_MS.SUSPECTED_CONFLICT_TAG],
        ))
        c2 = MemoryEntry(
            type=MemoryType.USER, key="home:zip", content="邮编100000",
            source="model_inferred", tags=[_MS.SUSPECTED_CONFLICT_TAG],
        )
        c3 = MemoryEntry(
            type=MemoryType.USER, key="home:addr", content="其实住上海",
            source="model_inferred", tags=[_MS.SUSPECTED_CONFLICT_TAG],
        )
        store._entries[c2.id] = c2
        store._entries[c3.id] = c3
        llm = _llm_returning("adjudicate", {"verdict": "a_wins", "explanation": "x"})
        detector = MagicMock()
        detector.get_unresolved = AsyncMock(return_value=[
            Contradiction(id="k1", memory_id_a=c1.id, memory_id_b=c3.id),
        ])
        detector.resolve = AsyncMock()
        engine = ReflectionEngine(_svc(store), llm_call=llm, contradiction_detector=detector)
        stats = await engine.resolve_conflicts()
        assert stats["resolved"] == 1
        # c3 被 c1 顶替（真实冲突对），c2(邮编,无关) 未被牵连。
        assert store.get(c3.id).is_superseded
        assert not store.get(c2.id).is_superseded

    @pytest.mark.asyncio
    async def test_conflict_pairs_consumes_unresolved_without_tag(self, store):
        """detector 检出的 unresolved 行(两端条目都无 suspected_conflict tag)应能被
        _conflict_pairs 配对裁决——不再要求 tag 交集。"""
        from echo_agent.memory.types import Contradiction
        a = store.add(MemoryEntry(
            type=MemoryType.USER, key="home:city", content="住北京",
            source="model_inferred",
        ))
        b = MemoryEntry(
            type=MemoryType.USER, key="home:addr", content="其实住上海",
            source="model_inferred",
        )
        store._entries[b.id] = b  # 绕过守卫构造并存冲突对
        # 两端都无 suspected_conflict tag。
        from echo_agent.memory.store import MemoryStore as _MS
        assert _MS.SUSPECTED_CONFLICT_TAG not in a.tags
        assert _MS.SUSPECTED_CONFLICT_TAG not in b.tags
        detector = MagicMock()
        detector.get_unresolved = AsyncMock(return_value=[
            Contradiction(id="k1", memory_id_a=a.id, memory_id_b=b.id),
        ])
        detector.resolve = AsyncMock()
        engine = ReflectionEngine(_svc(store), llm_call=AsyncMock(), contradiction_detector=detector)
        pairs = await engine._conflict_pairs()
        assert len(pairs) == 1
        paired_ids = frozenset((pairs[0][0].id, pairs[0][1].id))
        assert paired_ids == frozenset((a.id, b.id))


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
