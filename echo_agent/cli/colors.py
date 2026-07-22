"""ANSI color utilities for CLI output.

Color is emitted only when the output stream is a real TTY and the user has
not opted out via ``NO_COLOR`` (https://no-color.org/). When stdout is a pipe
or file, ANSI escapes would corrupt the captured text, so ``color()`` returns
plain strings instead. Callers that produce machine-readable output (``--json``)
can force this off with ``set_color_override(False)``.
"""

from __future__ import annotations

import os
import sys

# Explicit on/off switch. ``None`` means "auto-detect from the environment".
# Tests and ``--json`` output paths flip this to a fixed value.
_COLOR_OVERRIDE: bool | None = None


class Colors:
    RESET = "\033[0m"
    BOLD = "\033[1m"
    DIM = "\033[2m"
    RED = "\033[31m"
    GREEN = "\033[32m"
    YELLOW = "\033[33m"
    BLUE = "\033[34m"
    MAGENTA = "\033[35m"
    CYAN = "\033[36m"


def set_color_override(enabled: bool | None) -> None:
    """Force color on (True), off (False), or restore auto-detection (None)."""
    global _COLOR_OVERRIDE
    _COLOR_OVERRIDE = enabled


def color_enabled() -> bool:
    """Whether ANSI escapes should be emitted for the current stdout."""
    if _COLOR_OVERRIDE is not None:
        return _COLOR_OVERRIDE
    if os.environ.get("NO_COLOR"):
        return False
    try:
        return bool(sys.stdout.isatty())
    except (AttributeError, ValueError):
        # A detached/closed stream (e.g. some test harnesses) — play it safe.
        return False


def color(text: str, *codes: str) -> str:
    if not codes or not color_enabled():
        return text
    return "".join(codes) + text + Colors.RESET


def print_header(text: str) -> None:
    print(color(f"\n  {text}", Colors.BOLD, Colors.CYAN))
    print(color("  " + "─" * len(text), Colors.DIM))


def print_success(text: str) -> None:
    print(color(f"  ✓ {text}", Colors.GREEN))


def print_info(text: str) -> None:
    print(color(f"  {text}", Colors.DIM))


def print_warning(text: str) -> None:
    print(color(f"  ! {text}", Colors.YELLOW))


def print_error(text: str) -> None:
    print(color(f"  ✗ {text}", Colors.RED))
