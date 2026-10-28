"""Shared palette contract for setup and the Textual CLI."""

from echo_agent.cli.palette import DARK_PALETTE, LIGHT_PALETTE, active_palette, ansi
from echo_agent.cli.tui.theme import ECHO_THEME, ECHO_THEME_LIGHT


def test_tui_themes_source_the_shared_cli_palette():
    assert ECHO_THEME.primary == DARK_PALETTE["primary"]
    assert ECHO_THEME.success == DARK_PALETTE["success"]
    assert ECHO_THEME_LIGHT.primary == LIGHT_PALETTE["primary"]
    assert ECHO_THEME_LIGHT.warning == LIGHT_PALETTE["warning"]


def test_active_palette_and_ansi_follow_terminal_theme():
    assert active_palette({"ECHO_TUI_THEME": "dark"}) == DARK_PALETTE
    assert active_palette({"ECHO_TUI_THEME": "light"}) == LIGHT_PALETTE
    assert ansi("primary", {"ECHO_TUI_THEME": "dark"}) == "\033[38;2;79;209;197m"
