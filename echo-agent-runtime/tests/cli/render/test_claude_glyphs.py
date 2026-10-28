from echo_agent.cli.tui.glyphs import CLAUDE, NARROW, cog_glyph, resolve_glyphs


def test_resolve_claude_by_env():
    assert resolve_glyphs({"ECHO_TUI_ICONS": "claude"}) is CLAUDE


def test_unknown_value_still_falls_back_to_narrow():
    assert resolve_glyphs({"ECHO_TUI_ICONS": "nope"}) is NARROW


def test_claude_uses_filled_circle_for_actors():
    assert CLAUDE.reply == "⏺"
    assert CLAUDE.tool == "⏺"


def test_claude_uses_hook_for_child_lines():
    assert CLAUDE.branch_last == "⎿ "


def test_claude_rail_is_blank_by_design():
    # inline indents from render.geometry constants, never from a rail prefix.
    assert CLAUDE.rail == ""
    assert CLAUDE.branch == ""


def test_claude_cognitive_map_covers_every_narrow_key():
    assert set(CLAUDE.cognitive) == set(NARROW.cognitive)


def test_cog_glyph_accepts_explicit_set():
    assert cog_glyph("thinking", CLAUDE) == CLAUDE.cognitive["thinking"]


def test_cog_glyph_unknown_type_is_neutral_dot():
    assert cog_glyph("brand_new_type", CLAUDE) == "·"
