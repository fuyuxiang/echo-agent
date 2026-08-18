"""取回契约:notice 承诺"完整结果可取回",这里验证它对任何产物都成立。

spill 的全部价值就压在这一条上。它此前不成立的两种形状——单行超长输出、
超过搜索工具体积上限的大产物——都是真实高频场景(压缩日志、minified JSON、
大 build 输出),故各自留一条测试钉住。
"""

from __future__ import annotations

import asyncio
import json

import pytest

from echo_agent.agent.tools.read_spill import _MAX_LIMIT, ReadSpillTool
from echo_agent.spill.policy import SpillPolicy
from echo_agent.spill.store import SpillStore
from echo_agent.tools.base import ToolExecutionContext, ToolResult

SESSION = "sess-a"


def _ctx(session_key: str = SESSION) -> ToolExecutionContext:
    return ToolExecutionContext(execution_id="e", trace_id="t", session_key=session_key)


def _run(coro):
    return asyncio.run(coro)


def _spill(tmp_path, text: str, tool_name: str = "exec"):
    """跑一遍真实的 spill 链路,返回 (预览结果, 取回工具)。"""
    store = SpillStore(tmp_path)
    policy = SpillPolicy(store, max_inline_chars=6000)
    result = policy.apply(tool_name, SESSION, ToolResult(output=text))
    return result, ReadSpillTool(spill_root=tmp_path)


def _path_from(result: ToolResult) -> str:
    # 走 metadata 而不是解析 notice 文本:测的是取回能力,不是文案格式。
    return result.metadata["spill_path"]


def test_single_line_output_tail_is_reachable(tmp_path):
    """单行 JSON:按行分页永远读不到尾部,按字符可以。

    这是 read_file 取回方案的致命缺陷——lines[offset:offset+limit] 对只有一行
    的文本,offset=0 给全部、offset=1 给空,尾部结论无从抵达。
    """
    payload = json.dumps({"items": [{"i": i} for i in range(20000)], "verdict": "TAIL_VERDICT"})
    assert "\n" not in payload
    result, tool = _spill(tmp_path, payload)
    assert result.metadata["spilled"] is True

    path = _path_from(result)
    total = _run(tool.execute({"path": path, "limit": 1}, _ctx())).metadata["total_chars"]
    assert total == len(payload)
    tail = _run(tool.execute({"path": path, "offset": total - 200, "limit": 200}, _ctx()))
    assert tail.success
    assert "TAIL_VERDICT" in tail.output


def test_large_artifact_is_searchable(tmp_path):
    """1.1 MB 产物:此前 search_files 因 _MAX_FILE_BYTES 直接跳过,搜不到。"""
    text = ("filler line\n" * 100_000) + "NEEDLE_AT_END\n"
    assert len(text) > 1_100_000
    result, tool = _spill(tmp_path, text)
    res = _run(tool.execute({"path": _path_from(result), "pattern": "NEEDLE_AT_END"}, _ctx()))
    assert res.success
    assert "NEEDLE_AT_END" in res.output
    assert res.metadata["count"] == 1


def test_search_reports_offsets_usable_for_reading(tmp_path):
    """检索给出的位置就是字符 offset,可直接喂回读取。"""
    text = "x" * 50_000 + "MARKER" + "y" * 50_000
    result, tool = _spill(tmp_path, text)
    path = _path_from(result)
    hit = _run(tool.execute({"path": path, "pattern": "MARKER"}, _ctx()))
    offset = int(hit.output.split("@", 1)[1].split(":", 1)[0])
    read = _run(tool.execute({"path": path, "offset": offset, "limit": 10}, _ctx()))
    assert read.output.startswith("MARKER")


def test_paging_covers_whole_artifact(tmp_path):
    """按 next_offset 一路翻到底,拼回来必须与原文完全一致。"""
    text = "".join(f"{i:07d}" for i in range(4000))
    result, tool = _spill(tmp_path, text)
    path = _path_from(result)

    chunks: list[str] = []
    offset = 0
    for _ in range(100):
        res = _run(tool.execute({"path": path, "offset": offset, "limit": 5000}, _ctx()))
        assert res.success
        nxt = res.metadata.get("next_offset")
        # 续读提示是给模型看的,不属于内容,按返回的字符数裁掉。
        chunks.append(res.output[:res.metadata["returned_chars"]])
        if nxt is None:
            break
        offset = nxt
    assert "".join(chunks) == text


def test_offset_past_end_is_not_an_error(tmp_path):
    """越界读不该报 failure:模型据此判断"读完了",报错会让它以为出了问题。"""
    result, tool = _spill(tmp_path, "z" * 20_000)
    res = _run(tool.execute({"path": _path_from(result), "offset": 999_999}, _ctx()))
    assert res.success
    assert res.metadata["returned_chars"] == 0


def test_limit_is_capped(tmp_path):
    """模型要 100 万字符也只给上限那么多,否则一次取回就把上下文撑爆。"""
    result, tool = _spill(tmp_path, "z" * 200_000)
    res = _run(tool.execute({"path": _path_from(result), "limit": 1_000_000}, _ctx()))
    assert res.metadata["returned_chars"] <= _MAX_LIMIT


def test_invalid_regex_is_validation_error(tmp_path):
    result, tool = _spill(tmp_path, "z" * 20_000)
    res = _run(tool.execute({"path": _path_from(result), "pattern": "([unclosed"}, _ctx()))
    assert not res.success
    assert res.error_kind == "validation"


def test_stderr_heavy_failure_is_retrievable(tmp_path):
    """失败结果落的是 error 字段,取回链路同样要通——构建/测试失败正是大输出主场。"""
    store = SpillStore(tmp_path)
    policy = SpillPolicy(store, max_inline_chars=6000)
    trace = ("  File \"x.py\", line 1\n" * 5000) + "AssertionError: FINAL_CAUSE\n"
    result = policy.apply("exec", SESSION, ToolResult(success=False, error=trace))
    assert result.metadata["spilled"] is True

    tool = ReadSpillTool(spill_root=tmp_path)
    res = _run(tool.execute({"path": _path_from(result), "pattern": "FINAL_CAUSE"}, _ctx()))
    assert res.success
    assert "FINAL_CAUSE" in res.output


def test_read_spill_output_is_never_itself_spilled():
    """read_spill 必须在 SKIP_TOOLS 里。

    否则取回结果又被替换成预览,模型陷入"读取回提示的取回提示",完整内容
    永远拿不到——取回通道自己把自己堵死。
    """
    assert "read_spill" in SpillPolicy.SKIP_TOOLS


def test_read_spill_result_passes_through_policy_untouched(tmp_path):
    store = SpillStore(tmp_path)
    policy = SpillPolicy(store, max_inline_chars=100)
    big = "z" * 50_000
    out = policy.apply("read_spill", SESSION, ToolResult(output=big))
    assert out.output == big
    assert "spilled" not in out.metadata


@pytest.mark.parametrize("profile", ["minimal", "messaging", "coding", "full"])
def test_read_spill_survives_every_tool_profile(profile):
    """spill 对所有 profile 生效,取回工具就得对所有 profile 可见。

    minimal/messaging 恰是 exec 关掉的部署——那里没有 shell 兜底,取回工具被
    策略过滤掉就等于产物彻底不可达。
    """
    from echo_agent.config.schema import Config
    from echo_agent.security.tool_policy import is_tool_allowed

    config = Config()
    config.tools.profile = profile
    assert is_tool_allowed(config, ReadSpillTool(spill_root=None))
