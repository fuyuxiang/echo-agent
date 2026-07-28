"""Tests for the TUI experience features: live activity line, theme detection,
brand config, /help catalog, gradient banner, and diff coloring."""

from __future__ import annotations

from echo_agent.cli.tui.activity_line import ActivityLine, _fmt_elapsed
from echo_agent.cli.tui.brand import Brand, load_brand
from echo_agent.cli.tui.blocks import Banner, colorize_diff
from echo_agent.cli.tui.completion import COMMANDS, help_text
from echo_agent.cli.tui.theme import detect_light_mode, resolve_theme_name


# ── live activity line ───────────────────────────────────────────────
#
# Replaces the old rotating status phrases ("马上就好" / "还在跑" / …): they said
# nothing about what was happening and re-rendered on every beat. The line now
# carries stage, tool, elapsed time and in-flight count instead.

def _line() -> ActivityLine:
    """An ActivityLine without a live screen: render_text is pure, and the state
    transitions only touch `display`/`update`, which are stubbed here."""
    al = ActivityLine.__new__(ActivityLine)
    al._active = False
    al._stage = ""
    # Running tools are tracked by call id so out-of-order completions remove the
    # right one; see test_activity_line_names_the_tool_still_running.
    al._tools = {}
    al._started = None
    al._frame = 0
    al._timer = None
    al._mounted = False
    return al


def test_activity_line_hidden_until_a_turn_starts():
    al = _line()
    assert al.is_active is False
    assert al.render_text() == ""


def test_activity_line_names_stage_and_elapsed():
    al = _line()
    al.start()
    al._started = 100.0
    al.set_stage("thinking")
    out = al.render_text(now=112.0)
    assert "思考中" in out
    assert "12s" in out
    assert "Ctrl+C" in out


def test_activity_line_prefers_running_tool_over_stage():
    """A named running tool beats the phase label: "调用工具 读取" tells the user
    more than "调用工具"."""
    al = _line()
    al.start()
    al.set_stage("thinking")
    al.tool_started("read_file")
    out = al.render_text()
    assert "读取" in out
    assert "思考中" not in out


def test_activity_line_counts_concurrent_tools_and_drops_name_when_done():
    al = _line()
    al.start()
    al.tool_started("read_file", "c1")
    al.tool_started("shell", "c2")
    assert "2 个工具进行中" in al.render_text()
    al.tool_finished("c1")
    al.tool_finished("c2")
    out = al.render_text()
    # No tool left → back to the phase label, never a finished tool's name.
    assert "个工具进行中" not in out
    assert al._tools == {}


def test_activity_line_names_the_tool_still_running():
    """Parallel calls rarely finish in start order. The line must name whatever
    is still running, not the last one that happened to start: with a bare count
    plus "last name", finishing B first left the row advertising B."""
    al = _line()
    al.start()
    al.tool_started("read_file", "c1")
    al.tool_started("shell", "c2")
    al.tool_finished("c2")
    out = al.render_text()
    assert "读取" in out
    assert "命令" not in out and "shell" not in out


def test_activity_line_tolerates_finish_without_id():
    """A finish frame that carries no id must still drain the count rather than
    leaving the row stuck on a tool that already ended."""
    al = _line()
    al.start()
    al.tool_started("read_file", "c1")
    al.tool_finished()
    assert al._tools == {}


def test_activity_line_stop_clears_state():
    """A settled turn's history belongs in the transcript, so the row hides
    instead of freezing on a stale phase."""
    al = _line()
    al.start()
    al.set_stage("generating")
    al.stop()
    assert al.is_active is False
    assert al.render_text() == ""


def test_activity_line_revives_on_a_heartbeat_it_never_saw_start():
    """Heartbeats can arrive for work this client never saw begin (a turn
    accepted before a reconnect), so a stage update must revive the row."""
    al = _line()
    al.set_stage("calling_tool")
    assert al.is_active is True
    assert "调用工具" in al.render_text()


def test_elapsed_formatting_scales_with_duration():
    assert _fmt_elapsed(1.24) == "1.2s"
    assert _fmt_elapsed(42.6) == "42s"
    assert _fmt_elapsed(185) == "3m 5s"
    assert _fmt_elapsed(120) == "2m"


# ── theme detection ──────────────────────────────────────────────────

def test_theme_explicit_override_wins():
    assert detect_light_mode({"ECHO_TUI_THEME": "light"}) is True
    assert detect_light_mode({"ECHO_TUI_THEME": "dark"}) is False


def test_theme_light_boolean_flag():
    assert detect_light_mode({"ECHO_TUI_LIGHT": "1"}) is True
    assert detect_light_mode({"ECHO_TUI_LIGHT": "off"}) is False


def test_theme_colorfgbg_light_and_dark():
    assert detect_light_mode({"COLORFGBG": "0;15"}) is True   # slot 15 → light
    assert detect_light_mode({"COLORFGBG": "15;0"}) is False  # slot 0 → dark


def test_theme_apple_terminal_defaults_light():
    assert detect_light_mode({"TERM_PROGRAM": "Apple_Terminal"}) is True


def test_theme_defaults_dark():
    assert detect_light_mode({}) is False
    assert resolve_theme_name({}) == "echo"
    assert resolve_theme_name({"ECHO_TUI_THEME": "light"}) == "echo-light"


# ── brand config ─────────────────────────────────────────────────────

def test_brand_defaults():
    b = load_brand({})
    assert b.name == "echo"
    assert b.prompt == "❯"


def test_brand_env_override():
    b = load_brand({"ECHO_BRAND_NAME": "Acme", "ECHO_BRAND_GOODBYE": "bye"})
    assert b.name == "Acme"
    assert b.goodbye == "bye"


def test_brand_rejects_overlong_value():
    b = load_brand({"ECHO_BRAND_NAME": "x" * 200})
    assert b.name == Brand().name  # falls back to default


# ── /help catalog ────────────────────────────────────────────────────

def test_help_lists_every_command():
    txt = help_text()
    for cmd in COMMANDS:
        assert cmd.name in txt


def test_help_command_is_registered():
    assert any(c.name == "/help" for c in COMMANDS)
    assert any(c.name == "/theme" for c in COMMANDS)


# ── banner ───────────────────────────────────────────────────────────

def test_banner_default_uses_block_logo():
    text = Banner("sess_x").build_text()
    assert "sess_x" in text
    assert "█" in text  # block-letter art present


def test_banner_custom_brand_name_falls_back_to_wordmark():
    text = Banner(name="Acme", tagline="bot", welcome="hi").build_text()
    assert "Acme" in text
    assert "bot" in text


# ── diff coloring ────────────────────────────────────────────────────

def test_colorize_diff_tags_added_and_removed():
    out = colorize_diff("+new line\n-old line\n context")
    assert "[$success]+new line[/]" in out
    assert "[$error]-old line[/]" in out


def test_colorize_diff_caps_output():
    big = "\n".join(f"+line{i}" for i in range(100))
    out = colorize_diff(big, max_lines=10)
    assert "还有 90 行" in out
