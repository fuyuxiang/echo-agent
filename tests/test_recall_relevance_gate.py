"""召回相关性门控:候选须在"至少一路"有判别力的信号上达标才进结果。

病根:RRF 是纯 rank-based,丢弃了向量余弦这一绝对相关性信号;BM25 侧又无
下限,导致仅共享单个常见汉字(的/是/我…)的噪声记忆也能进池、被配额硬凑进
prompt。这里固化:纯单字命中不足以让候选进结果;共享判别性 token(latin 词 /
CJK bigram)或向量达标才进。
"""

from __future__ import annotations

from unittest.mock import AsyncMock, MagicMock

import pytest

from echo_agent.memory.forgetting import ForgettingCurve
from echo_agent.memory.retrieval import HybridRetriever
from echo_agent.memory.store import MemoryStore
from echo_agent.memory.types import Episode, MemoryEntry, MemoryType


def _entry(eid, key, content, importance=0.5):
    return MemoryEntry(
        id=eid, type=MemoryType.USER, key=key, content=content, importance=importance
    )


class TestLexicalRelevanceGate:
    @pytest.mark.asyncio
    async def test_single_common_char_overlap_rejected(self):
        """仅共享单个常见汉字("的")的噪声,不应被召回——无向量、纯 BM25。"""
        strong = _entry("strong", "db", "数据库查询优化的方法")
        # 与 query 只共享单字"的",无任何判别性 bigram
        noise = _entry("noise", "pet", "我的宠物很可爱")
        retriever = HybridRetriever(
            entries_fn=lambda: [strong, noise], forgetting=ForgettingCurve(),
        )
        results = await retriever.retrieve("查询优化的技巧", limit=5)
        ids = [r.id for r, _ in results]
        assert "strong" in ids, "共享 bigram(查询/优化)的强相关项应召回"
        assert "noise" not in ids, "仅共享单字'的'的噪声不应被召回"

    @pytest.mark.asyncio
    async def test_no_relevant_entry_returns_empty_not_padded(self):
        """库中无相关项时应返回空,而非拿弱相关凑满配额。"""
        noises = [
            _entry("n1", "k1", "我的宠物很可爱"),
            _entry("n2", "k2", "今天天气不错的"),
            _entry("n3", "k3", "他是一个好人的"),
        ]
        retriever = HybridRetriever(
            entries_fn=lambda: noises, forgetting=ForgettingCurve(),
        )
        results = await retriever.retrieve("数据库索引原理", limit=5)
        assert results == [], "无判别性重叠时应返回空,不得硬凑"

    @pytest.mark.asyncio
    async def test_vector_relevant_admitted_without_lexical(self):
        """无词面重叠但向量达标的候选应被召回(向量是独立的达标通路)。"""
        e = _entry("v1", "topic", "完全不同用词的内容")
        vec = MagicMock()
        vec.search = AsyncMock(return_value=[("v1", 0.92)])
        retriever = HybridRetriever(
            entries_fn=lambda: [e], vector_index=vec,
            embed_fn=AsyncMock(return_value=[0.1, 0.2]),
            forgetting=ForgettingCurve(), min_similarity=0.3,
        )
        results = await retriever.retrieve("毫不相干的查询词", limit=5)
        assert [r.id for r, _ in results] == ["v1"]

    @pytest.mark.asyncio
    async def test_discriminative_bigram_still_recalls(self):
        """共享判别性 bigram 的中文查询仍正常召回(不误杀真召回)。"""
        entries = [
            _entry("e1", "coffee", "用户喜欢喝咖啡"),
            _entry("e2", "loc", "用户住在北京"),
        ]
        retriever = HybridRetriever(
            entries_fn=lambda: entries, forgetting=ForgettingCurve(),
        )
        results = await retriever.retrieve("咖啡", limit=5)
        assert results and results[0][0].id == "e1"


class TestSearchScoredGate:
    def test_single_char_overlap_rejected_in_fallback(self, tmp_path):
        """兜底 search_scored 同样不因单字命中而召回噪声。"""
        store = MemoryStore(memory_dir=tmp_path / "mem")
        store.add(MemoryEntry(type=MemoryType.USER, key="s", content="数据库查询优化的方法"))
        store.add(MemoryEntry(type=MemoryType.USER, key="n", content="我的宠物很可爱"))
        results = store.search_scored("查询优化的技巧", limit=5)
        ids = [e.key for e, _ in results]
        assert "s" in ids
        assert "n" not in ids, "兜底路径也不应因单字'的'召回噪声"
