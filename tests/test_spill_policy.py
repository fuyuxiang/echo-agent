"""spill 策略：.text 语义分派、跳过规则、best-effort 降级,以及三个已确认漏口的回归。

三个漏口都是现存缺陷:MCP 拼接无界(tool_adapter.py:86)、exec 的 stderr
无界(shell.py:151)、exec 失败路径 error=err_output 无界。构建失败和 pytest
失败正是最常见的大输出场景,却完全没有上限。
"""

from __future__ import annotations

from pathlib import Path
from unittest.mock import patch

import pytest

from echo_agent.spill.policy import SpillPolicy
from echo_agent.spill.store import SpillStore
from echo_agent.tools import ToolResult

_BIG = "q" * 60000


def _policy(tmp_path, **kw):
    return SpillPolicy(SpillStore(tmp_path), max_inline_chars=kw.pop("cap", 6000), **kw)


def test_success_spills_output_and_leaves_error(tmp_path):
    r = ToolResult(success=True, output=_BIG, error="")
    out = _policy(tmp_path).apply("exec", "s", r)
    assert len(out.output) <= 6000
    assert out.error == ""
    assert out.metadata["spilled"] is True
    assert out.metadata["omitted_chars"] > 0


def test_failure_spills_error_and_leaves_output(tmp_path):
    # 回归:exec 失败时模型读的是 .text -> "Error: {error}",此前完全无界
    r = ToolResult(success=False, output="kept", error=_BIG)
    out = _policy(tmp_path).apply("exec", "s", r)
    assert len(out.error) <= 6000
    assert out.output == "kept"
    assert out.metadata["spilled"] is True


def test_mcp_unbounded_output_is_bounded(tmp_path):
    # 回归:MCP 动态注册,逐工具打补丁覆盖不到
    r = ToolResult(success=True, output=_BIG, metadata={"mcp_server": "x"})
    out = _policy(tmp_path).apply("mcp_x_y", "s", r)
    assert len(out.output) <= 6000
    assert out.metadata["mcp_server"] == "x"


def test_short_result_untouched(tmp_path):
    r = ToolResult(success=True, output="short")
    out = _policy(tmp_path).apply("exec", "s", r)
    assert out.output == "short"
    assert "spilled" not in out.metadata


def test_read_file_is_skipped(tmp_path):
    # 否则形成 read -> spill -> 再 read 死循环
    r = ToolResult(success=True, output=_BIG)
    out = _policy(tmp_path).apply("read_file", "s", r)
    assert out.output == _BIG


def test_search_files_is_not_skipped(tmp_path):
    r = ToolResult(success=True, output=_BIG)
    out = _policy(tmp_path).apply("search_files", "s", r)
    assert len(out.output) <= 6000


def test_disabled_policy_is_noop(tmp_path):
    r = ToolResult(success=True, output=_BIG)
    out = _policy(tmp_path, enabled=False).apply("exec", "s", r)
    assert out.output == _BIG


@pytest.mark.parametrize("exc", [PermissionError("denied"), OSError("ENOSPC")])
def test_save_failure_keeps_inline_result(tmp_path, exc):
    # best-effort:落盘失败绝不能把成功的调用变成失败,也不能藏掉内联结果
    r = ToolResult(success=True, output=_BIG)
    with patch.object(SpillStore, "save", side_effect=exc):
        out = _policy(tmp_path).apply("exec", "s", r)
    assert out.output == _BIG
    assert out.success is True
    assert "spilled" not in out.metadata


def test_full_text_is_recoverable_from_disk(tmp_path):
    r = ToolResult(success=True, output=_BIG)
    out = _policy(tmp_path).apply("exec", "s", r)
    assert Path(out.metadata["spill_path"]).read_text(encoding="utf-8") == _BIG
