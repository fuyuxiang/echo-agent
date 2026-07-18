"""RRF 融合 + episodic 统一排序。"""
from __future__ import annotations
from unittest.mock import AsyncMock, MagicMock

import pytest

from echo_agent.memory.retrieval import HybridRetriever
from echo_agent.memory.types import MemoryEntry, MemoryType, Episode
from echo_agent.memory.forgetting import ForgettingCurve


def _entry(eid, key, content, importance=0.5):
    return MemoryEntry(id=eid, type=MemoryType.USER, key=key, content=content, importance=importance)


def _episode(eid, summary, session_key="s1"):
    return Episode(id=eid, session_key=session_key, summary=summary)


class TestRRFFusion:
    @pytest.mark.asyncio
    async def test_dual_signal_beats_single_signal(self):
        """条目同时被 BM25 和向量排前面 able RRF 分数高于只被一路排前面的。"""
        e1 = _entry("e1", "python", "Python编程语言")
        e2 = _entry("e2", "java", "Java编程")
        entries = [e1, e2]

        async def embed(text):
            if "Python" in text:
                return [1.0, 0.0]
            return [0.5, 0.5]

        vi = MagicMock()
        vi.search = AsyncMock(return_value=[("e1", 0.95), ("e2", 0.4)])

        retriever = HybridRetriever(
            entries_fn=lambda: entries, vector_index=vi,
            forgetting=ForgettingCurve(), embed_fn=embed,
        )
        results = await retriever.retrieve("Python编程", limit=5)
        assert results[0][0].id == "e1"

    @pytest.mark.asyncio
    async def test_importance_decay_affects_ranking(self):
        """importance 低的条目即使排名高也会被拉下来。"""
        e_high = _entry("eh", "python", "Python高手", importance=0.9)
        e_low = _entry("el", "python", "Python新手", importance=0.05)
        entries = [e_high, e_low]

        retriever = HybridRetriever(
            entries_fn=lambda: entries, forgetting=ForgettingCurve(),
        )
        results = await retriever.retrieve("Python", limit=5)
        assert results[0][0].id == "eh"

    @pytest.mark.asyncio
    async def test_single_signal_degradation(self):
        """向量不可用时纯 BM25 RRF 仍正常。"""
        e1 = _entry("e1", "rust", "Rust系统编程")
        retriever = HybridRetriever(
            entries_fn=lambda: [e1], forgetting=ForgettingCurve(),
        )
        results = await retriever.retrieve("Rust", limit=5)
        assert len(results) == 1 and results[0][0].id == "e1"

    @pytest.mark.asyncio
    async def test_no_entropy_method_remains(self):
        """_query_entropy 已删除。"""
        assert not hasattr(HybridRetriever, "_query_entropy")
        assert not hasattr(HybridRetriever, "_resonance_score")


class TestEpisodicInRetrieval:
    @pytest.mark.asyncio
    async def test_episode_ranks_with_memory(self):
        """语义相近的 episode 排在无关 memory 前面。"""
        e_unrelated = _entry("e1", "cooking", "做饭技巧烹饪指南")
        ep_related = _episode("ep1", "讨论了项目部署方案")
        entries = [e_unrelated]

        async def embed(text):
            if "部署" in text or "上线" in text:
                return [1.0, 0.0]
            return [0.0, 1.0]

        vi = MagicMock()
        vi.search = AsyncMock(return_value=[("ep:ep1", 0.9), ("e1", 0.1)])

        retriever = HybridRetriever(
            entries_fn=lambda: entries, vector_index=vi,
            forgetting=ForgettingCurve(), embed_fn=embed,
        )
        results = await retriever.retrieve("上线", limit=8, episodes=[ep_related])
        assert any(
            isinstance(r, Episode) and r.id == "ep1"
            for r, _ in results[:2]
        )

    @pytest.mark.asyncio
    async def test_quota_memory_max_5_episode_max_3(self):
        """配额截断：memory ≤5、episode ≤3。"""
        entries = [_entry(f"e{i}", f"k{i}", f"内容{i}") for i in range(10)]
        episodes = [_episode(f"ep{i}", f"摘要{i}") for i in range(5)]

        retriever = HybridRetriever(
            entries_fn=lambda: entries, forgetting=ForgettingCurve(),
        )
        results = await retriever.retrieve("内容 摘要", limit=8, episodes=episodes)
        mem_count = sum(1 for r, _ in results if isinstance(r, MemoryEntry))
        ep_count = sum(1 for r, _ in results if isinstance(r, Episode))
        assert mem_count <= 5
        assert ep_count <= 3
        assert mem_count + ep_count <= 8

    @pytest.mark.asyncio
    async def test_no_episodes_param_backward_compat(self):
        """不传 episodes 时行为与纯 memory 一致。"""
        e = _entry("e1", "test", "测试内容")
        retriever = HybridRetriever(
            entries_fn=lambda: [e], forgetting=ForgettingCurve(),
        )
        results = await retriever.retrieve("测试", limit=5)
        assert results and results[0][0].id == "e1"


class TestEpisodeSearchFnAutoAssembly:
    """episode_search_fn 让 episode 候选在 retrieve() 内部自动组装——
    这是 CLI degrade 命中缓存也能召回 episode 的关键（prefetch 走同一 retrieve）。"""

    @pytest.mark.asyncio
    async def test_auto_assembles_episodes_when_not_passed(self):
        """不显式传 episodes 时调用 episode_search_fn 并把结果并入排序。"""
        e_unrelated = _entry("e1", "cooking", "做饭技巧")
        ep_related = _episode("ep1", "讨论了项目部署上线方案")

        search_fn = AsyncMock(return_value=[ep_related])
        retriever = HybridRetriever(
            entries_fn=lambda: [e_unrelated], forgetting=ForgettingCurve(),
            episode_search_fn=search_fn,
        )
        results = await retriever.retrieve("上线部署", limit=8, episode_session_key="s1")
        search_fn.assert_awaited_once()
        # episode_session_key 透传给检索函数，episode 进入结果。
        assert search_fn.await_args.args[1] == "s1"
        assert any(isinstance(r, Episode) and r.id == "ep1" for r, _ in results)

    @pytest.mark.asyncio
    async def test_explicit_episodes_skip_search_fn(self):
        """调用方已显式传 episodes 时不再调用 search_fn，避免重复召回。"""
        search_fn = AsyncMock(return_value=[])
        retriever = HybridRetriever(
            entries_fn=lambda: [], forgetting=ForgettingCurve(),
            episode_search_fn=search_fn,
        )
        await retriever.retrieve("q", limit=8, episodes=[_episode("ep1", "x")])
        search_fn.assert_not_awaited()

    @pytest.mark.asyncio
    async def test_mem_type_filter_skips_episode_search(self):
        """按记忆类型过滤时不组装 episode（episode 不属于单一 memory type）。"""
        search_fn = AsyncMock(return_value=[])
        retriever = HybridRetriever(
            entries_fn=lambda: [], forgetting=ForgettingCurve(),
            episode_search_fn=search_fn,
        )
        await retriever.retrieve("q", limit=8, mem_type=MemoryType.USER)
        search_fn.assert_not_awaited()

    @pytest.mark.asyncio
    async def test_search_fn_failure_degrades_gracefully(self):
        """episode_search_fn 抛异常时退化为纯 memory，不冒泡。"""
        e = _entry("e1", "k", "内容")
        search_fn = AsyncMock(side_effect=RuntimeError("boom"))
        retriever = HybridRetriever(
            entries_fn=lambda: [e], forgetting=ForgettingCurve(),
            episode_search_fn=search_fn,
        )
        results = await retriever.retrieve("内容", limit=8, episode_session_key="s1")
        assert results and results[0][0].id == "e1"
