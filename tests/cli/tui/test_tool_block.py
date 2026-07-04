from echo_agent.cli.tui.blocks import humanize_tool, pick_object, summarize_result


def test_humanize_known_and_unknown():
    assert humanize_tool("read_file") == "读取"
    assert humanize_tool("search_files") == "搜索"
    assert humanize_tool("exec") == "执行"
    # 未知工具名原样兜底，不硬造中文
    assert humanize_tool("some_new_tool") == "some_new_tool"


def test_pick_object_by_tool_type():
    assert pick_object("read_file", {"path": "a/b/inference_stage.py"}) == "inference_stage.py"
    assert pick_object("list_dir", {"path": "echo_agent/cli"}) == "echo_agent/cli"
    assert pick_object("search_files", {"pattern": "tool_call"}) == '"tool_call"'
    assert pick_object("exec", {"command": "find . -name x"}).startswith("find")
    # 兜底：第一个字符串参数
    assert pick_object("weird", {"n": 3, "q": "hello"}) == "hello"
    # 无可用参数不崩
    assert pick_object("read_file", {}) == ""


def test_summarize_result_uses_meta_not_text():
    # read_file: 用 result_meta 的真实行数，而非在截断文本上重数
    assert summarize_result("read_file", {"total_lines": 300}, "只有\n三行\n预览", True) == "300 行"
    assert summarize_result("search_files", {"count": 40}, "", True) == "找到 40 处"
    assert summarize_result("list_dir", {"count": 12}, "", True) == "12 个"
    assert summarize_result("exec", None, "done", True) == "完成"
    # 失败优先
    assert summarize_result("read_file", {"total_lines": 5}, "", False) == "失败"
    # 无 meta 兜底截预览
    assert summarize_result("unknown", None, "some output text", True) == "some output text"
    # None/缺键/空文本都不崩
    assert isinstance(summarize_result("read_file", None, "", True), str)
    assert isinstance(summarize_result("read_file", {}, "", True), str)


def test_tool_block_running_then_done_flip():
    from echo_agent.cli.tui.blocks import ToolCallBlock

    b = ToolCallBlock("tc_1", "read_file", {"path": "x/inference_stage.py"})
    running = b.render_summary()
    assert "🔧" in running and "读取" in running and "inference_stage.py" in running
    assert running.endswith("…")           # 进行中：尾部省略号
    assert "✓" not in running

    b.mark_done("ok", {"total_lines": 300}, "…preview…", 320)
    done = b.render_summary()
    assert done.endswith("✓")
    assert "300 行" in done
    assert "…" not in done                  # 完成后去掉省略号


def test_tool_block_error_shows_cross():
    from echo_agent.cli.tui.blocks import ToolCallBlock

    b = ToolCallBlock("tc_2", "exec", {"command": "rm -rf /tmp/x"})
    b.mark_done("err", None, "Error: boom", 10)
    s = b.render_summary()
    assert s.endswith("✗")
    assert "失败" in s


def test_tool_block_detail_has_params_and_result():
    from echo_agent.cli.tui.blocks import ToolCallBlock

    b = ToolCallBlock("tc_3", "read_file", {"path": "a.py", "limit": 300})
    b.mark_done("ok", {"total_lines": 42}, "line1\nline2", 5)
    assert b.expanded is False
    b.toggle()
    assert b.expanded is True
    detail = b.render_detail()
    assert "参数" in detail and "a.py" in detail
    assert "结果" in detail


import pytest
from echo_agent.cli.tui.protocol import CogEvent


def _tool_ev(tcid, status, name="read_file", **data):
    d = {"tool_call_id": tcid, "name": name, "params": {"path": "a.py"},
         "status": status, **data}
    return CogEvent("tool_call", f"evt_{tcid}_{status}", "in_1", d, "")


@pytest.mark.asyncio
async def test_add_tool_call_flips_in_place():
    from textual.app import App
    from echo_agent.cli.tui.transcript import TranscriptView

    class T(App):
        def compose(self):
            yield TranscriptView()

    app = T()
    async with app.run_test():
        tv = app.query_one(TranscriptView)
        b1 = tv.add_tool_call(_tool_ev("tc_1", "running"))
        assert b1.status == "running"
        assert tv.tool_block_count == 1
        # 同一 id 的完成事件：原地翻转，不新增
        b2 = tv.add_tool_call(
            _tool_ev("tc_1", "ok", result_meta={"total_lines": 42}, result_text="x")
        )
        assert b2 is b1
        assert b1.status == "ok"
        assert tv.tool_block_count == 1
        # 不同 id：新增
        tv.add_tool_call(_tool_ev("tc_2", "running"))
        assert tv.tool_block_count == 2
        tv.clear()
        assert tv.tool_block_count == 0
