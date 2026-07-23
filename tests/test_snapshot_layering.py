"""快照分层:常驻核心 = 显式 pinned ∪ top-K by importance;长尾不进快照,
改由 query 驱动的召回按需接管(上一轮加固的 retrieve() 已是天然 query 门)。

病根:USER≤50 + ENV≤30 无条件全量注入 system prompt,不看当前问题——是"召回
看着不相关"的第二条路径(与 RRF 那条独立)。分层后每轮 prompt 只带极小核心,
其余靠相关性召回带出。
"""

from __future__ import annotations

from pathlib import Path

import pytest

from echo_agent.memory.store import MemoryStore
from echo_agent.memory.types import MemoryEntry, MemoryType


def _store(tmp_path: Path, **kw) -> MemoryStore:
    return MemoryStore(memory_dir=tmp_path / "mem", **kw)


def _add(store, *, mid, content, mtype=MemoryType.USER, importance=0.8,
         access=3, pinned=False):
    e = MemoryEntry(id=mid, type=mtype, key=mid, content=content,
                    importance=importance, pinned=pinned)
    e.touch()
    e.access_count = access
    store.add(e)
    return e


class TestPinnedField:
    def test_pinned_round_trips(self):
        e = MemoryEntry(type=MemoryType.USER, key="k", content="c", pinned=True)
        assert MemoryEntry.from_dict(e.to_dict()).pinned is True

    def test_pinned_defaults_false(self):
        assert MemoryEntry(type=MemoryType.USER, key="k", content="c").pinned is False

    def test_from_dict_legacy_without_pinned(self):
        """旧数据无 pinned 字段 → 默认 False,不炸。"""
        legacy = {"id": "x", "type": "user", "key": "k", "content": "c"}
        assert MemoryEntry.from_dict(legacy).pinned is False


class TestPinnedWiringThroughService:
    async def _mk(self, tmp_path):
        from echo_agent.memory.service import ActorContext, MemoryService
        store = MemoryStore(memory_dir=tmp_path / "mem", scope_policy="session",
                            snapshot_layering=True, snapshot_user_core_max=2)
        return MemoryService(store), store, ActorContext

    @pytest.mark.asyncio
    async def test_service_add_persists_pinned_and_enters_core(self, tmp_path):
        """service.add(pinned=True) → 落库 pinned,且低分也进核心。"""
        svc, store, ActorContext = await self._mk(tmp_path)
        ctx = ActorContext(actor="model", session_key="s", memory_scope="sc")
        # 两条高分占满 core_max=2
        await svc.add(ctx, type=MemoryType.USER, key="h1", content="高分1",
                      importance=0.9, source="user_stated")
        await svc.add(ctx, type=MemoryType.USER, key="h2", content="高分2",
                      importance=0.85, source="user_stated")
        # 一条低分但 pinned
        r = await svc.add(ctx, type=MemoryType.USER, key="pin", content="过敏史",
                          importance=0.3, source="user_stated", pinned=True)
        assert r.ok and r.entry.pinned is True
        _text, ids = store.get_snapshot_with_ids(session_key="sc")
        assert r.entry.id in ids, "pinned 低分条目应挤进核心"


class TestSnapshotLayering:
    def test_core_capped_to_user_max(self, tmp_path):
        """开启分层后,USER 核心只保留 top-K(按 importance),长尾不进快照。"""
        store = _store(tmp_path, snapshot_layering=True, snapshot_user_core_max=3)
        for i in range(10):
            _add(store, mid=f"u{i}", content=f"fact {i}",
                 importance=0.5 + i * 0.03, access=2)
        _text, ids = store.get_snapshot_with_ids()
        user_ids = [i for i in ids if i.startswith("u")]
        assert len(user_ids) == 3, "USER 核心应被截到 3 条"
        # 最高 importance 的 u9/u8/u7 应入选,最低的 u0 不入选
        assert "u9" in user_ids and "u0" not in user_ids

    def test_pinned_always_in_core_even_if_low_importance(self, tmp_path):
        """显式 pinned 的低分条目也必须进核心(挤掉一个 top-K 名额)。"""
        store = _store(tmp_path, snapshot_layering=True, snapshot_user_core_max=2)
        _add(store, mid="hi1", content="high1", importance=0.9, access=5)
        _add(store, mid="hi2", content="high2", importance=0.85, access=5)
        _add(store, mid="pin", content="pinned low", importance=0.3, access=1,
             pinned=True)
        _text, ids = store.get_snapshot_with_ids()
        assert "pin" in ids, "pinned 条目必须进核心,无视 importance 排名"

    def test_env_core_capped_separately(self, tmp_path):
        store = _store(tmp_path, snapshot_layering=True, snapshot_env_core_max=2)
        for i in range(6):
            _add(store, mid=f"e{i}", content=f"env {i}",
                 mtype=MemoryType.ENVIRONMENT, importance=0.5 + i * 0.05, access=1)
        _text, ids = store.get_snapshot_with_ids()
        env_ids = [i for i in ids if i.startswith("e")]
        assert len(env_ids) == 2

    def test_layering_off_keeps_legacy_full_snapshot(self, tmp_path):
        """关闭分层 → 回退旧行为(全量,受既有 50/30 上限约束)。"""
        store = _store(tmp_path, snapshot_layering=False)
        for i in range(10):
            _add(store, mid=f"u{i}", content=f"fact {i}", importance=0.7, access=2)
        _text, ids = store.get_snapshot_with_ids()
        user_ids = [i for i in ids if i.startswith("u")]
        assert len(user_ids) == 10, "关闭分层应保留全部(未超 50 上限)"
