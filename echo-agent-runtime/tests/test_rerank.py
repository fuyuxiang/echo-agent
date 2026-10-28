"""Cross-encoder 精排:RRF 融合后对 top-K 候选按 query-doc 相关性重排。

RRF 是纯 rank-based,只融合两路的顺序、不懂"这条到底和问题多相关"。cross-encoder
直接对 (query, doc) 打相关性分,是精排金标准。这里固化:注入 rerank_fn 后,融合结
果按 cross-encoder 分重排;超时/失败/未注入时保持原 RRF 顺序(纯增强,不改主干)。
"""

from __future__ import annotations

import pytest

from echo_agent.memory.forgetting import ForgettingCurve
from echo_agent.memory.retrieval import HybridRetriever
from echo_agent.memory.types import MemoryEntry, MemoryType


def _entry(eid, key, content, importance=0.5):
    return MemoryEntry(
        id=eid, type=MemoryType.USER, key=key, content=content, importance=importance
    )


class TestRerankReordering:
    @pytest.mark.asyncio
    async def test_rerank_reorders_by_cross_encoder_score(self):
        """cross-encoder 认为 e2 更相关 → e2 排到 e1 前,推翻 RRF 原序。"""
        e1 = _entry("e1", "python", "Python 编程语言")
        e2 = _entry("e2", "python", "Python 数据分析")
        # rerank_fn: e2 分更高
        async def rerank_fn(query, docs):
            return [0.2 if "编程" in d else 0.9 for d in docs]

        retriever = HybridRetriever(
            entries_fn=lambda: [e1, e2], forgetting=ForgettingCurve(),
            rerank_fn=rerank_fn, rerank_top_k=10,
        )
        results = await retriever.retrieve("Python", limit=5)
        assert [r.id for r, _ in results][:2] == ["e2", "e1"]

    @pytest.mark.asyncio
    async def test_rerank_failure_falls_back_to_rrf_order(self):
        """rerank_fn 抛异常 → 保持 RRF 原序,不冒泡。"""
        e1 = _entry("e1", "python", "Python 编程 最相关内容 查询词命中")
        e2 = _entry("e2", "java", "无关内容")

        async def boom(query, docs):
            raise RuntimeError("reranker down")

        retriever = HybridRetriever(
            entries_fn=lambda: [e1, e2], forgetting=ForgettingCurve(),
            rerank_fn=boom,
        )
        results = await retriever.retrieve("Python 编程", limit=5)
        assert results and results[0][0].id == "e1"

    @pytest.mark.asyncio
    async def test_no_rerank_fn_keeps_rrf(self):
        """未注入 rerank_fn 时行为与原 RRF 完全一致。"""
        e1 = _entry("e1", "rust", "Rust 系统编程")
        retriever = HybridRetriever(
            entries_fn=lambda: [e1], forgetting=ForgettingCurve(),
        )
        results = await retriever.retrieve("Rust", limit=5)
        assert results and results[0][0].id == "e1"

    @pytest.mark.asyncio
    async def test_rerank_min_score_drops_low_relevance(self):
        """低于 rerank_min_score 的候选在重排后被剔除。"""
        e1 = _entry("e1", "k1", "查询 相关 内容")
        e2 = _entry("e2", "k2", "查询 边缘 内容")

        async def rerank_fn(query, docs):
            return [0.8 if "相关" in d else 0.05 for d in docs]

        retriever = HybridRetriever(
            entries_fn=lambda: [e1, e2], forgetting=ForgettingCurve(),
            rerank_fn=rerank_fn, rerank_min_score=0.3,
        )
        results = await retriever.retrieve("查询 内容", limit=5)
        ids = [r.id for r, _ in results]
        assert "e1" in ids and "e2" not in ids


class TestLocalReranker:
    @pytest.mark.asyncio
    async def test_rerank_returns_scores_aligned_to_docs(self):
        """LocalReranker.rerank 返回与 docs 对齐的分数列表;模型不可用时返回 None。"""
        from echo_agent.memory.local_rerank import LocalReranker

        r = LocalReranker(model_name="BAAI/bge-reranker-base")
        # 未真正加载模型时(available 但无模型且加载失败/超时)应返回 None,不抛
        r._closed = True  # 强制不可用路径
        out = await r.rerank("q", ["d1", "d2"])
        assert out is None
