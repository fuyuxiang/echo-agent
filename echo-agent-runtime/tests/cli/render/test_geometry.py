from echo_agent.cli.render.geometry import (
    child_prefix, cont_prefix, head_prefix, reply_body_indent,
)
from echo_agent.cli.tui.glyphs import CLAUDE


def test_head_line_starts_at_column_zero():
    assert head_prefix() == ""


def test_child_line_indents_two():
    assert child_prefix() == "  "


def test_continuation_aligns_under_child_content():
    # "  " + "⎿ " puts child content at column 4; a continuation line must
    # land there too, without redrawing the hook.
    assert len(cont_prefix()) == len(child_prefix()) + len(CLAUDE.branch_last)
    assert cont_prefix().strip() == ""


def test_reply_body_indent_clears_the_sigil():
    # "⏺ " is two cells, so wrapped answer lines align under the text.
    assert reply_body_indent() == "  "


def test_prefixes_are_whitespace_only():
    for p in (head_prefix(), child_prefix(), cont_prefix()):
        assert p.strip() == ""
