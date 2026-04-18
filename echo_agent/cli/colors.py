"""ANSI color utilities for CLI output."""

from __future__ import annotations


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


def color(text: str, *codes: str) -> str:
    if not codes:
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
