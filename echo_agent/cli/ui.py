"""questionary-backed interactive primitives with a prompt.py fallback.

Every primitive works in two backends:
  - rich: questionary (arrow-key select, multiselect, spinner) when a TTY is
    present and questionary imports cleanly.
  - fallback: echo_agent.cli.prompt input() helpers otherwise (non-TTY, SSH
    with no PTY, or questionary missing) — never raises on account of the UI.
"""
from __future__ import annotations

import sys
from contextlib import contextmanager
from typing import Iterator

from echo_agent.cli.colors import (
    Colors, color, print_error, print_info, print_success, print_warning,
)
from echo_agent.cli.prompt import (
    is_interactive, prompt, prompt_checklist, prompt_choice, prompt_yes_no,
)

Choice = tuple[str, str, str]  # (value, label, hint)

try:
    import questionary as _q  # type: ignore
    _HAS_Q = True
except Exception:  # pragma: no cover - import guard
    _q = None  # type: ignore
    _HAS_Q = False


# Shared visual identity for every interactive prompt. Centralized here so the
# whole setup wizard looks consistent — tweak once, all menus follow.
_POINTER = "❯"
_QMARK = "◆"

if _HAS_Q:
    _STYLE = _q.Style([
        ("qmark", "fg:#00afaf bold"),          # leading marker (cyan)
        ("question", "bold"),                  # the prompt text
        ("pointer", "fg:#00afaf bold"),        # arrow on the focused row
        ("highlighted", "fg:#00afaf bold"),    # focused choice label
        ("selected", "fg:#00af5f"),            # chosen value (green)
        ("separator", "fg:#6c6c6c bold"),      # group headers (dim, weighty)
        ("answer", "fg:#00afaf bold"),         # echoed answer after submit
        ("instruction", "fg:#6c6c6c"),         # (Use arrow keys) hint
        ("disabled", "fg:#6c6c6c italic"),
    ])
else:  # pragma: no cover - import guard
    _STYLE = None


def use_rich() -> bool:
    return _HAS_Q and is_interactive()


def _labels(choices: list[Choice]) -> list[str]:
    return [f"{lbl}  {hint}".rstrip() if hint else lbl for _v, lbl, hint in choices]


def _default_index(choices: list[Choice], default: str) -> int:
    for i, (v, _l, _h) in enumerate(choices):
        if v == default:
            return i
    return 0


def select(message: str, choices: list[Choice], default: str = "") -> str:
    if use_rich():
        opts = [_q.Choice(title=lbl if not hint else f"{lbl}  ({hint})", value=v)
                for v, lbl, hint in choices]
        default_val = default or choices[0][0]
        ans = _q.select(message, choices=opts, default=default_val,
                        style=_STYLE, pointer=_POINTER, qmark=_QMARK).ask()
        if ans is None:
            sys.exit(0)
        return ans
    idx = prompt_choice(message, _labels(choices), default=_default_index(choices, default))
    return choices[idx][0]


def select_grouped(message: str, groups: list[tuple[str, list[Choice]]], default: str = "") -> str:
    if use_rich():
        opts: list = []
        for group_label, gchoices in groups:
            opts.append(_q.Separator(f"── {group_label} ──"))
            for v, lbl, hint in gchoices:
                opts.append(_q.Choice(title=lbl if not hint else f"{lbl}  ({hint})", value=v))
        default_val = default or (groups[0][1][0][0] if groups and groups[0][1] else "")
        ans = _q.select(message, choices=opts, default=default_val,
                        style=_STYLE, pointer=_POINTER, qmark=_QMARK).ask()
        if ans is None:
            sys.exit(0)
        return ans
    flat: list[Choice] = []
    labels: list[str] = []
    for group_label, gchoices in groups:
        for v, lbl, hint in gchoices:
            flat.append((v, lbl, hint))
            labels.append(f"[{group_label}] {lbl}")
    idx = prompt_choice(message, labels, default=_default_index(flat, default))
    return flat[idx][0]


def multiselect(message: str, choices: list[Choice], preselected: list[str] | None = None) -> list[str]:
    pre = set(preselected or [])
    if use_rich():
        opts = [_q.Choice(title=lbl if not hint else f"{lbl}  ({hint})", value=v, checked=v in pre)
                for v, lbl, hint in choices]
        ans = _q.checkbox(message, choices=opts, style=_STYLE,
                          pointer=_POINTER, qmark=_QMARK).ask()
        if ans is None:
            sys.exit(0)
        return list(ans)
    pre_idx = [i for i, (v, _l, _h) in enumerate(choices) if v in pre]
    idxs = prompt_checklist(message, _labels(choices), pre_selected=pre_idx)
    return [choices[i][0] for i in idxs]


def text(message: str, default: str = "") -> str:
    if use_rich():
        ans = _q.text(message, default=default, style=_STYLE, qmark=_QMARK).ask()
        if ans is None:
            sys.exit(0)
        return ans.strip() or default
    return prompt(message, default=default)


def password(message: str) -> str:
    if use_rich():
        ans = _q.password(message, style=_STYLE, qmark=_QMARK).ask()
        if ans is None:
            sys.exit(0)
        return ans.strip()
    return prompt(message, password=True)


def confirm(message: str, default: bool = True) -> bool:
    if use_rich():
        ans = _q.confirm(message, default=default, style=_STYLE, qmark=_QMARK).ask()
        if ans is None:
            sys.exit(0)
        return bool(ans)
    return prompt_yes_no(message, default=default)


def note(message: str, kind: str = "info") -> None:
    {"success": print_success, "warning": print_warning,
     "error": print_error, "info": print_info}.get(kind, print_info)(message)


def intro(title: str) -> None:
    print()
    print(color(f"  ◆  {title}", Colors.CYAN, Colors.BOLD))


def outro(message: str) -> None:
    print()
    print(color(f"  ◆  {message}", Colors.CYAN, Colors.BOLD))
    print()


@contextmanager
def spinner(message: str) -> Iterator[None]:
    print(color(f"  ⋯ {message}", Colors.DIM))
    try:
        yield
    finally:
        pass
