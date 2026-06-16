"""M4-5 CJK retrieval regressions."""

from __future__ import annotations

import pytest

from echo_agent.memory.retrieval import HybridRetriever
from echo_agent.memory.text import cjk_tokens
from echo_agent.memory.types import MemoryEntry, MemoryType


def test_cjk_tokens_chars_and_bigrams():
    toks = cjk_tokens("记忆")
    assert "记" in toks
    assert "忆" in toks
    assert "记忆" in toks


def test_cjk_tokens_empty_for_latin():
    assert cjk_tokens("hello world") == []


def test_cjk_tokens_only_cjk_segment_in_mixed():
    toks = cjk_tokens("agent记忆")
    assert "记" in toks and "忆" in toks and "记忆" in toks
    assert "agent" not in toks
    assert all(not t.isascii() for t in toks)


def test_query_entropy_chinese_not_constant():
    # 修复前纯中文查询恒为 0.5（latin-only 正则匹配不到 CJK）。
    # 修复后熵基于实际 CJK token 分布，低多样性与高多样性应不同。
    low = HybridRetriever._query_entropy("记 记 记")
    high = HybridRetriever._query_entropy("北京 上海 广州 深圳 杭州")
    assert low != 0.5 or high != 0.5
    assert high > low


def _zh_entries() -> list[MemoryEntry]:
    return [
        MemoryEntry(type=MemoryType.USER, key="pref", content="用户喜欢喝咖啡"),
        MemoryEntry(type=MemoryType.USER, key="loc", content="用户住在北京"),
        MemoryEntry(type=MemoryType.USER, key="en", content="user likes tea"),
    ]


@pytest.mark.asyncio
async def test_bm25_recalls_chinese_query():
    entries = _zh_entries()
    retriever = HybridRetriever(entries_fn=lambda: entries, vector_index=None)
    results = await retriever.retrieve("咖啡", limit=5)
    assert results, "中文查询应能召回（BM25 不再恒空）"
    top = results[0][0]
    assert "咖啡" in top.content
    assert results[0][1] > 0


@pytest.mark.asyncio
async def test_english_query_still_works():
    entries = _zh_entries()
    retriever = HybridRetriever(entries_fn=lambda: entries, vector_index=None)
    results = await retriever.retrieve("tea", limit=5)
    assert results
    assert "tea" in results[0][0].content
