import io

from echo_agent.cli.inline.printer import InlinePrinter
from echo_agent.cli.render import ansi as A


def _printer():
    """A printer over a plain StringIO: not a tty, so no escapes and no spinner
    animation. Keeps assertions about text content free of colour noise."""
    buf = io.StringIO()
    A.set_color_override(False)
    return InlinePrinter(stream=buf), buf


def teardown_function():
    A.set_color_override(None)


def test_construction_writes_nothing():
    p, buf = _printer()
    assert buf.getvalue() == ""


def test_head_uses_glyph_at_column_zero():
    p, buf = _printer()
    p.head("读取 config.py")
    assert buf.getvalue() == "⏺ 读取 config.py\n"


def test_child_indents_two_and_hooks():
    p, buf = _printer()
    p.child("210 行")
    assert buf.getvalue() == "  ⎿ 210 行\n"


def test_cont_aligns_under_child_content():
    p, buf = _printer()
    p.cont("更多说明")
    assert buf.getvalue() == "    更多说明\n"


def test_blank_collapses_consecutive_calls():
    # Content first: leading blanks are suppressed outright (see
    # test_leading_blank_is_suppressed), so collapsing is only observable once
    # there is something for the gap to separate from.
    p, buf = _printer()
    p.head("做事")
    p.blank()
    p.blank()
    p.blank()
    assert buf.getvalue() == "⏺ 做事\n\n"


def test_blank_after_content_emits_one_line():
    p, buf = _printer()
    p.head("做事")
    p.blank()
    assert buf.getvalue() == "⏺ 做事\n\n"


def test_leading_blank_is_suppressed():
    # Nothing printed yet: an opening blank line would push the first content
    # down for no reason.
    p, buf = _printer()
    p.blank()
    assert buf.getvalue() == ""


def test_tool_line_running_shows_pending():
    p, buf = _printer()
    p.tool_line("read_file", {"path": "/a/config.py"}, "running")
    out = buf.getvalue()
    assert "⏺ 读取 config.py" in out
    assert "…" in out


def test_tool_line_done_shows_summary_and_mark():
    p, buf = _printer()
    p.tool_line(
        "read_file", {"path": "/a/config.py"}, "ok",
        result_meta={"total_lines": 210},
    )
    out = buf.getvalue()
    assert "⏺ 读取 config.py" in out
    assert "  ⎿ 210 行" in out


def test_tool_line_failure_marks_and_words_it():
    p, buf = _printer()
    p.tool_line("exec", {"command": "false"}, "fail", result_text="boom")
    out = buf.getvalue()
    assert "失败" in out or "✗" in out


def test_tool_line_masks_secret_params():
    p, buf = _printer()
    p.tool_line(
        "web_fetch", {"url": "https://x.test?token=sk-abcdefgh1234"}, "ok",
    )
    assert "sk-abcdefgh1234" not in buf.getvalue()


def test_spinner_is_static_on_non_tty():
    p, buf = _printer()
    p.spinner_start("正在组织答案")
    p.spinner_update("正在组织答案 1.0s")
    out = buf.getvalue()
    assert "\r" not in out
    assert "正在组织答案" in out


def test_spinner_clear_on_non_tty_writes_nothing_extra():
    p, buf = _printer()
    p.spinner_start("跑着")
    before = buf.getvalue()
    p.spinner_clear()
    assert buf.getvalue() == before


def test_output_after_spinner_starts_on_its_own_line():
    p, buf = _printer()
    p.spinner_start("跑着")
    p.head("做事")
    lines = buf.getvalue().splitlines()
    assert lines[-1] == "⏺ 做事"


def test_is_tty_false_for_stringio():
    p, _ = _printer()
    assert p.is_tty is False
