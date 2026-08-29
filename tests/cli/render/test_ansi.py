import pytest

from echo_agent.cli import palette
from echo_agent.cli.render import ansi as A


def test_fg_returns_24bit_escape():
    out = A.fg("primary")
    assert out.startswith("\033[38;2;")
    assert out.endswith("m")


def test_fg_rejects_unknown_role():
    with pytest.raises(KeyError):
        A.fg("chartreuse")


def test_paint_wraps_with_reset_when_color_on():
    A.set_color_override(True)
    try:
        out = A.paint("hi", A.BOLD)
        assert out == f"{A.BOLD}hi{A.RESET}"
    finally:
        A.set_color_override(None)


def test_paint_is_identity_when_color_off():
    A.set_color_override(False)
    try:
        assert A.paint("hi", A.BOLD, A.fg("error")) == "hi"
    finally:
        A.set_color_override(None)


def test_paint_without_codes_is_identity():
    A.set_color_override(True)
    try:
        assert A.paint("hi") == "hi"
    finally:
        A.set_color_override(None)


def test_supports_color_follows_override():
    A.set_color_override(False)
    try:
        assert A.supports_color() is False
    finally:
        A.set_color_override(None)


def _escape(hex_value: str) -> str:
    red, green, blue = (int(hex_value[i:i + 2], 16) for i in (1, 3, 5))
    return f"\033[38;2;{red};{green};{blue}m"


def test_reset_palette_cache_forces_reresolution(monkeypatch):
    # /theme flips light/dark at runtime; without the hook the cache would keep
    # serving the palette resolved at first paint.
    monkeypatch.setenv("ECHO_TUI_THEME", "dark")
    try:
        A.reset_palette_cache()
        assert A.fg("primary") == _escape(palette.DARK_PALETTE["primary"])

        monkeypatch.setenv("ECHO_TUI_THEME", "light")
        # Still the dark escape: the cache is doing its job.
        assert A.fg("primary") == _escape(palette.DARK_PALETTE["primary"])

        A.reset_palette_cache()
        assert A.fg("primary") == _escape(palette.LIGHT_PALETTE["primary"])
    finally:
        # Leave no cached palette behind for tests that follow.
        A.reset_palette_cache()
