from echo_agent.cli.render.tool import (
    fmt_duration_ms, humanize_tool, pick_object, summarize_result,
)


def test_humanize_known_tool():
    assert humanize_tool("read_file") == "读取"


def test_humanize_unknown_tool_falls_back_to_id():
    assert humanize_tool("some_new_tool") == "some_new_tool"


def test_pick_object_uses_basename_for_paths():
    assert pick_object("read_file", {"path": "/a/b/config.py"}) == "config.py"


def test_pick_object_quotes_search_pattern():
    assert pick_object("search_files", {"pattern": "load_config"}) == '"load_config"'


def test_pick_object_falls_back_to_first_string_param():
    assert pick_object("mystery", {"n": 3, "thing": "hello"}) == "hello"


def test_summarize_result_uses_producer_count():
    assert summarize_result("read_file", {"total_lines": 210}, "", True) == "210 行"


def test_summarize_result_reports_failure():
    assert summarize_result("read_file", None, "boom", False) == "失败"


def test_fmt_duration_drops_subsecond():
    assert fmt_duration_ms(400) == ""


def test_fmt_duration_shows_seconds():
    assert fmt_duration_ms(12400) == "12.4s"


def test_fmt_duration_shows_minutes():
    assert fmt_duration_ms(90000) == "1m 30s"


def test_fmt_duration_handles_none():
    assert fmt_duration_ms(None) == ""
