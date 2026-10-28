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
    al._outcome = ""
    al._final_elapsed = 0.0
    al._tools_seen = 0
    al._counted = set()
    al._stopping = False
    return al


def test_activity_line_blank_before_the_first_turn():
    """Nothing has run yet, so there is nothing to report — a fresh screen ends
    at the last real content. (After a turn finishes the row is NOT blank; see
    test_activity_line_settles_into_a_completion_line.)"""
    al = _line()
    assert al.is_active is False
    assert al.is_settled is False
    assert al.render_text() == ""


def test_activity_line_settles_into_a_completion_line():
    """The row must NOT vanish when a turn ends.

    Hiding it removed the only moving thing on screen along with the elapsed
    clock, and the row collapsed to zero height — so "finished" and "hung" were
    the same picture, and users reported not being able to tell whether the agent
    was done or stuck. A settled line names the outcome and the duration.
    """
    al = _line()
    al.start()
    al._started = 100.0
    al.tool_started("read_file", "c1")
    al.tool_finished("c1")
    al.settle("done")
    out = al.render_text()
    assert al.is_active is False
    assert al.is_settled is True
    assert "完成" in out
    # The duration survives the settle: it retroactively explains the wait.
    assert al._final_elapsed > 0
    # Tools the turn ran are counted even though _tools drained as they finished.
    assert "1 个工具" in out
    # Nothing is running, so the interrupt hint would be a lie.
    assert "Ctrl+C" not in out


def test_activity_line_settle_distinguishes_failure_from_success():
    """All four end paths used to call one `stop()`, so a socket drop looked
    exactly like a completed turn."""
    for outcome, expected in (
        ("error", "出错"),
        ("disconnected", "连接已断开"),
        ("interrupted", "已中断"),
    ):
        al = _line()
        al.start()
        al.settle(outcome)
        assert expected in al.render_text(), outcome
    # An unrecognised key degrades to a neutral label instead of raising on a
    # pure decoration path.
    al = _line()
    al.start()
    al.settle("something_new")
    assert "已结束" in al.render_text()


def test_activity_line_start_clears_the_previous_settled_line():
    """The row is about the current turn once one begins."""
    al = _line()
    al.start()
    al.settle("done")
    al.start()
    assert al.is_settled is False
    assert "完成" not in al.render_text()


def test_activity_line_settle_is_noop_before_any_turn():
    """A stray end-frame on a fresh screen must not invent a summary for a turn
    that never ran."""
    al = _line()
    al.settle("done")
    assert al.is_settled is False
    assert al.render_text() == ""


def test_activity_line_later_end_reason_wins():
    """The end paths overlap — a gateway error is routinely followed by the
    socket dropping — so the more specific later reason must not be dropped."""
    al = _line()
    al.start()
    al.settle("error")
    al.settle("disconnected")
    assert "连接已断开" in al.render_text()


def test_activity_line_acknowledges_a_requested_stop():
    """Ctrl+C is cooperative: the gateway only polls the flag at checkpoints, so
    the turn keeps running for a while. The row previously ignored the request
    entirely and kept spinning as if nothing had been asked."""
    al = _line()
    al.start()
    al.tool_started("shell", "c1")
    al.note_stopping()
    out = al.render_text()
    assert "正在停止" in out
    # The tool name must not keep claiming the user's attention over the stop.
    assert "命令" not in out
    # A second Ctrl+C is the exit guard, so stop advertising the interrupt.
    assert "Ctrl+C" not in out


def test_activity_line_cancelled_turn_does_not_report_success():
    """An interrupted turn converges through the ordinary reply path, so the
    normal `settle("done")` would credit a cancelled turn as 完成."""
    al = _line()
    al.start()
    al.note_stopping()
    al.settle("done")
    assert "已中断" in al.render_text()
    assert "完成" not in al.render_text()


def test_activity_line_hard_failure_outranks_a_requested_stop():
    """A gateway error is a harder fact than the user's intent to stop."""
    al = _line()
    al.start()
    al.note_stopping()
    al.settle("error")
    assert "出错" in al.render_text()


def test_activity_line_note_stopping_ignored_when_idle():
    al = _line()
    al.note_stopping()
    assert al._stopping is False
    assert al.render_text() == ""


def test_activity_line_reset_blanks_the_settled_line():
    """/clear wipes the transcript the summary described."""
    al = _line()
    al.start()
    al.settle("done")
    al.reset()
    assert al.is_settled is False
    assert al.render_text() == ""


def test_activity_line_counts_each_call_once():
    """A duplicate start frame for the same call must not inflate the count."""
    al = _line()
    al.start()
    al.tool_started("read_file", "c1")
    al.tool_started("read_file", "c1")
    al.settle("done")
    assert "1 个工具" in al.render_text()


def test_activity_line_counts_a_tool_seen_only_finishing():
    """Frames are not guaranteed to arrive in pairs — a reconnect mid-round
    replays a call's outcome without its start. Counting only start frames made
    the settled line under-report (or omit) work that had actually run."""
    al = _line()
    al.start()
    al.tool_started("exec", "c1")
    al.tool_finished("c1")
    al.tool_finished("c2")      # 只见终态帧
    al.settle("done")
    assert "2 个工具" in al.render_text()


def test_activity_line_does_not_double_count_a_paired_call():
    """The finish-frame tally must stay idempotent with the start frame, or every
    ordinary tool would count twice."""
    al = _line()
    al.start()
    al.tool_started("exec", "c1")
    al.tool_started("read_file", "c2")
    al.tool_finished("c1")
    al.tool_finished("c2")
    al.settle("done")
    assert "2 个工具" in al.render_text()


def test_activity_line_tool_count_resets_between_turns():
    """The tally is per turn: it must not carry the previous turn's calls."""
    al = _line()
    al.start()
    al.tool_started("exec", "c1")
    al.tool_finished("c1")
    al.settle("done")
    al.start()
    al.tool_started("read_file", "c9")
    al.settle("done")
    assert "1 个工具" in al.render_text()


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


def test_activity_line_stop_alias_still_settles():
    """`stop()` is the old spelling, kept because it reads correctly at the call
    sites ("the turn stopped"). It must settle the row rather than hide it —
    hiding was the original defect."""
    al = _line()
    al.start()
    al.set_stage("generating")
    al.stop()
    assert al.is_active is False
    # No longer blank, and no longer showing the stale "正在组织答案" phase.
    assert "完成" in al.render_text()
    assert "正在组织答案" not in al.render_text()


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
