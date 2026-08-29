from echo_agent.cli.render.text import clip, strip_legacy_glyph


def test_clip_collapses_whitespace():
    assert clip("a   b\n c", 40) == "a b c"


def test_clip_truncates_with_ellipsis():
    assert clip("abcdefghij", 5) == "abcd…"


def test_clip_keeps_short_text_intact():
    assert clip("abc", 5) == "abc"


def test_strip_legacy_glyph_removes_leading_emoji():
    assert strip_legacy_glyph("🧠 回忆起 3 条") == "回忆起 3 条"


def test_strip_legacy_glyph_leaves_plain_text():
    assert strip_legacy_glyph("回忆起 3 条") == "回忆起 3 条"
