"""M4-5 CJK retrieval regressions."""

from __future__ import annotations

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
