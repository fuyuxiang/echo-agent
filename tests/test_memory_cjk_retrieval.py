"""M4-5 CJK retrieval regressions."""

from __future__ import annotations

from echo_agent.memory.retrieval import HybridRetriever
from echo_agent.memory.text import cjk_tokens


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
