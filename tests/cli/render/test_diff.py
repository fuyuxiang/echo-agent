from echo_agent.cli.render.diff import (
    ANSI_DIFF_STYLE, TEXTUAL_DIFF_STYLE, colorize_diff,
)


def test_default_style_is_textual_markup():
    out = colorize_diff("+added\n-removed")
    assert "[$success]+added[/]" in out
    assert "[$error]-removed[/]" in out


def test_hunk_header_is_muted():
    out = colorize_diff("@@ -1,2 +1,3 @@")
    assert "[$text-muted]@@ -1,2 +1,3 @@[/]" in out


def test_triple_markers_are_not_added_or_removed():
    out = colorize_diff("+++ b/file\n--- a/file")
    assert "[$success]" not in out
    assert "[$error]" not in out
    assert out.count("[$text-muted]") == 2


def test_context_line_unstyled():
    assert colorize_diff(" context") == " context"


def test_caps_output_and_reports_remainder():
    big = "\n".join(f"+line{i}" for i in range(50))
    out = colorize_diff(big, max_lines=10)
    assert len(out.splitlines()) == 11
    assert "还有 40 行" in out


def test_ansi_style_emits_escapes_not_markup():
    out = colorize_diff("+added", style=ANSI_DIFF_STYLE)
    assert "\033[" in out
    assert "[$success]" not in out


def test_textual_style_escapes_brackets_in_content():
    out = colorize_diff("+text [not a tag]", style=TEXTUAL_DIFF_STYLE)
    assert "\\[" in out


def test_ansi_style_leaves_brackets_alone():
    out = colorize_diff("+text [literal]", style=ANSI_DIFF_STYLE)
    assert "[literal]" in out
