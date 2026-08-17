"""采集上限:各工具不再在 registry 之前把文本砍掉,stderr 也纳入上限。

shell.py 此前只对 stdout 应用 cap,err_output 原样拼接进 combined,且
return_code != 0 时 error=err_output —— 两处都无界。
"""

from __future__ import annotations

from echo_agent.agent.tools.shell import ShellTool
from echo_agent.config.schema import Config


def test_exec_default_cap_is_acquisition_sized():
    # 16000 是"丢数据的刀";2000000 是"传递上限",让 spill 能拿到完整文本
    assert Config().tools.exec.max_output_chars == 2000000


def test_shell_bounds_stderr_too():
    tool = ShellTool("/tmp", max_output=100)
    big = "e" * 5000
    bounded = tool._bound(big)
    assert len(bounded) <= 100 + 80  # cap 加截断标记的余量


def test_web_schema_has_no_max_chars_default():
    # schema 里的 default 同时是给模型看的提示。保留 16000 会让它误以为那是
    # 模型可见的量(实际由 spill.max_inline_chars 决定);写 2000000 又会诱导
    # 它传大值。移除是唯一诚实的选择。
    from echo_agent.agent.tools.web import WebFetchTool
    prop = WebFetchTool.parameters["properties"]["max_chars"]
    assert "default" not in prop
