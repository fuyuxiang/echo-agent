"""Spinner behaviour on the tty branch, which test_printer.py cannot reach.

Every case there writes to a plain StringIO, so the animated path is never
executed: dropping the spinner_clear() from _line(), degrading _CLEAR_LINE to a
bare "\\r", or freezing the frame index all leave that suite fully green. The
two hard constraints those mutations break — clear with carriage-return *plus*
erase-line, and never let content share a row with the spinner — are asserted
here.

A fake tty rather than a real pty: a pty translates "\\n" into "\\r\\n", so the
expected values would have to accommodate terminal line discipline and stop
showing the escape sequences under test.
"""

import io

from echo_agent.cli.inline.printer import InlinePrinter
from echo_agent.cli.render import ansi as A

_CL = "\r\033[2K"


class _FakeTTY(io.StringIO):
    def isatty(self):
        return True


def _printer():
    buf = _FakeTTY()
    # Colour off so the assertions read as the control characters they are
    # about; is_tty comes from the stream, not from the colour policy.
    A.set_color_override(False)
    return InlinePrinter(stream=buf), buf


def teardown_function():
    A.set_color_override(None)


def test_spinner_animates_on_tty():
    p, buf = _printer()
    p.spinner_start("跑")
    assert buf.getvalue() == f"{_CL}⠋ 跑"


def test_spinner_update_advances_the_frame():
    p, buf = _printer()
    p.spinner_start("跑")
    p.spinner_update("跑")
    assert buf.getvalue().endswith(f"{_CL}⠙ 跑")


def test_spinner_clear_erases_the_whole_row():
    # Carriage return alone would leave the tail of a longer previous label
    # sitting at the end of the row.
    p, buf = _printer()
    p.spinner_start("跑")
    p.spinner_clear()
    assert buf.getvalue().endswith(_CL)


def test_content_never_shares_a_row_with_the_spinner():
    p, buf = _printer()
    p.spinner_start("跑")
    p.head("做事")
    assert buf.getvalue() == f"{_CL}⠋ 跑{_CL}⏺ 做事\n"


def test_blank_also_clears_a_live_spinner():
    p, buf = _printer()
    p.head("做事")
    p.spinner_start("跑")
    p.blank()
    assert buf.getvalue() == f"⏺ 做事\n{_CL}⠋ 跑{_CL}\n"


def test_head_accepts_an_explicit_glyph():
    # Cognitive frames carry their own marker (✻ thinking, ✦ approval); the
    # prefix stays geometry's job, so they override the glyph rather than
    # assembling the row via plain().
    p, buf = _printer()
    p.head("思考中", glyph="✻")
    assert buf.getvalue() == "✻ 思考中\n"


def test_head_without_text_leaves_no_trailing_space():
    p, buf = _printer()
    p.head("")
    assert buf.getvalue() == "⏺\n"
