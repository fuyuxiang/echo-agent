"""预览合成：恒不超限、恒变短、头尾都在。

"头尾都在"那条是本次改动的核心价值——现有的只留头部截断,系统性地丢掉了
pytest 的 "=== N failed ===" 和堆栈最内层调用。没有这条测试兜住,日后有人
"优化"回只留头部不会有任何东西报警。
"""

from __future__ import annotations

import pytest

from echo_agent.spill.preview import compose

_LOCATOR = "/home/u/.echo-agent/data/spill/session-abc123def456/a1b2c3d4-exec.txt"


def test_keeps_both_head_and_tail():
    text = "HEAD_SENTINEL" + ("x" * 50000) + "TAIL_SENTINEL"
    out = compose(text, _LOCATOR, 6000)
    assert out is not None
    assert "HEAD_SENTINEL" in out
    assert "TAIL_SENTINEL" in out


@pytest.mark.parametrize("cap", [50, 100, 200, 500, 1000, 6000, 20000])
def test_never_exceeds_cap(cap):
    text = "y" * 100000
    out = compose(text, _LOCATOR, cap)
    if out is not None:
        assert len(out) <= cap


@pytest.mark.parametrize("cap", [50, 200, 6000])
def test_never_grows(cap):
    text = "y" * 100000
    out = compose(text, _LOCATOR, cap)
    if out is not None:
        assert len(out) < len(text)


def test_mentions_locator_and_retrieval_tools():
    out = compose("z" * 50000, _LOCATOR, 6000)
    assert _LOCATOR in out
    assert "read_file" in out
    assert "search_files" in out


def test_tiny_cap_returns_none_rather_than_oversized():
    # cap 小于 notice 本身长度时,不能吐出超限替换,只能保留原文
    assert compose("z" * 50000, _LOCATOR, 10) is None


def test_tail_gets_more_budget_than_head():
    text = "H" * 30000 + "T" * 30000
    out = compose(text, _LOCATOR, 6000)
    assert out.count("T") > out.count("H")
