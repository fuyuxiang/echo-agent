"""Tests for the TUI experience features: status phrases, theme detection,
brand config, /help catalog, gradient banner, and diff coloring."""

from __future__ import annotations

import random

from echo_agent.cli.tui.brand import Brand, load_brand
from echo_agent.cli.tui.blocks import Banner, colorize_diff
from echo_agent.cli.tui.completion import COMMANDS, help_text
from echo_agent.cli.tui.status_phrases import _PHRASES, choose_status_phrase
from echo_agent.cli.tui.theme import detect_light_mode, resolve_theme_name


# ── status phrases ───────────────────────────────────────────────────

def test_status_phrase_from_catalog():
    assert choose_status_phrase() in _PHRASES


def test_status_phrase_avoids_recent_repeats():
    rng = random.Random(0)
    recent: list[str] = []
    picks = [choose_status_phrase(recent, rng=rng) for _ in range(4)]
    # Within one recent-window (4) there must be no immediate repeats.
    assert len(set(picks)) == len(picks)


def test_status_phrase_recent_window_is_bounded():
    recent: list[str] = []
    for _ in range(50):
        choose_status_phrase(recent)
    assert len(recent) <= 4


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
