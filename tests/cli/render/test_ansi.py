import pytest

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
