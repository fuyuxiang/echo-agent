"""Interactive prompt utilities for CLI setup wizard."""

from __future__ import annotations

import getpass
import sys

from echo_agent.cli.colors import Colors, color, print_error


class PromptAborted(Exception):
    """The user (or a non-interactive stdin) ended the prompt session.

    Raised instead of calling ``sys.exit(0)`` inside the helper. Exiting with 0
    from here reported SUCCESS for work that never happened: pipe an empty stdin
    into `checkpoint restore` (or any confirm-gated command) and every prompt
    hit EOF immediately, the process exited 0, and a caller script read that as
    "restored". Callers now decide: the setup wizard still treats an abort as a
    clean user cancellation (exit 0), while a command whose action did not run
    reports a non-zero code.
    """


def is_interactive() -> bool:
    return sys.stdin.isatty()


def prompt(question: str, default: str = "", password: bool = False) -> str:
    if default:
        display = f"{question} [{default}]: "
    else:
        display = f"{question}: "
    try:
        if password:
            value = getpass.getpass(color(display, Colors.YELLOW))
        else:
            value = input(color(display, Colors.YELLOW))
        return value.strip() or default
    except (KeyboardInterrupt, EOFError) as e:
        print()
        raise PromptAborted(str(e) or type(e).__name__) from e


def prompt_yes_no(question: str, default: bool = True) -> bool:
    hint = "Y/n" if default else "y/N"
    while True:
        try:
            value = input(color(f"  {question} [{hint}]: ", Colors.YELLOW)).strip().lower()
        except (KeyboardInterrupt, EOFError) as e:
            print()
            raise PromptAborted(str(e) or type(e).__name__) from e
        if not value:
            return default
        if value in ("y", "yes"):
            return True
        if value in ("n", "no"):
            return False
        print_error("Please enter y or n")


def prompt_choice(question: str, choices: list[str], default: int = 0) -> int:
    print(color(f"\n  {question}", Colors.YELLOW))
    for i, choice in enumerate(choices):
        marker = "●" if i == default else "○"
        if i == default:
            print(color(f"    {i + 1}. {marker} {choice}", Colors.GREEN))
        else:
            print(f"    {i + 1}. {marker} {choice}")
    while True:
        try:
            value = input(color(f"  Select [1-{len(choices)}] (default {default + 1}): ", Colors.DIM)).strip()
        except (KeyboardInterrupt, EOFError) as e:
            print()
            raise PromptAborted(str(e) or type(e).__name__) from e
        if not value:
            return default
        try:
            idx = int(value) - 1
            if 0 <= idx < len(choices):
                return idx
            print_error(f"Please enter a number between 1 and {len(choices)}")
        except ValueError:
            print_error("Please enter a number")


def prompt_checklist(question: str, items: list[str], pre_selected: list[int] | None = None) -> list[int]:
    selected = set(pre_selected or [])
    print(color(f"\n  {question}", Colors.YELLOW))
    print(color("  Enter numbers to toggle, 'done' to confirm, 'none' to clear", Colors.DIM))
    while True:
        for i, item in enumerate(items):
            mark = "✓" if i in selected else " "
            line = f"    [{mark}] {i + 1}. {item}"
            if i in selected:
                print(color(line, Colors.GREEN))
            else:
                print(f"    [{mark}] {i + 1}. {item}")
        try:
            value = input(color("  Toggle/done: ", Colors.DIM)).strip().lower()
        except (KeyboardInterrupt, EOFError) as e:
            print()
            raise PromptAborted(str(e) or type(e).__name__) from e
        if value in ("done", "d", ""):
            return sorted(selected)
        if value == "none":
            selected.clear()
            continue
        try:
            idx = int(value) - 1
            if 0 <= idx < len(items):
                selected.symmetric_difference_update({idx})
            else:
                print_error(f"Please enter 1-{len(items)}")
        except ValueError:
            print_error("Enter a number, 'done', or 'none'")
