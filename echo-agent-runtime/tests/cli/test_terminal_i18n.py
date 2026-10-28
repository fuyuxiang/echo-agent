from echo_agent.cli.i18n import get_locale, set_locale
from echo_agent.cli.render.tool import humanize_risk, humanize_tool, summarize_result
from echo_agent.cli.tui.brand import load_brand
from echo_agent.cli.tui.details import DetailPrefs


def test_terminal_surface_uses_english_locale():
    saved = get_locale()
    set_locale("en")
    try:
        brand = load_brand({})
        assert brand.placeholder == "Type a message…"
        assert "commands" in brand.welcome
        assert humanize_tool("read_file") == "Read"
        assert humanize_risk("write") == "modifies data"
        assert summarize_result("read_file", {"total_lines": 12}, "", True) == "12 lines"
        assert DetailPrefs().describe() == [
            ("Thinking & memory", "collapsed"),
            ("Tool calls", "collapsed"),
            ("Activity", "hidden"),
        ]
    finally:
        set_locale(saved)


def test_terminal_surface_keeps_chinese_locale():
    saved = get_locale()
    set_locale("zh")
    try:
        assert load_brand({}).placeholder == "输入消息…"
        assert humanize_tool("read_file") == "读取"
        assert summarize_result("read_file", {"total_lines": 12}, "", True) == "12 行"
    finally:
        set_locale(saved)
