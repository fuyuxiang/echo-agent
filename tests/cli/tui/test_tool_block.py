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
