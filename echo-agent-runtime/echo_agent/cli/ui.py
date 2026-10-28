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

from echo_agent.cli.colors import Colors, color
from echo_agent.cli.palette import active_palette, ansi
from echo_agent.cli.prompt import (
    PromptAborted, is_interactive, prompt, prompt_checklist, prompt_choice,
    prompt_yes_no,
)

Choice = tuple[str, str, str]  # (value, label, hint)


def _aborted(what: str) -> PromptAborted:
    """questionary returns None when the user hits Ctrl-C / Ctrl-D.

    Mirrors the fallback backend: both raise PromptAborted so the caller decides
    the exit code. Previously the rich path called sys.exit(0) here, which made
    a cancelled prompt indistinguishable from completed work.
    """
    return PromptAborted(f"cancelled at {what}")

try:
    import questionary as _q  # type: ignore
    _HAS_Q = True
except Exception:  # pragma: no cover - import guard
    _q = None  # type: ignore
    _HAS_Q = False


# Shared visual identity for every interactive prompt. Centralized here so the
# whole setup wizard looks consistent — tweak once, all menus follow.
_POINTER = "❯"
_QMARK = "●"

if _HAS_Q:
    _palette = active_palette()
    _STYLE = _q.Style([
        ("qmark", f"fg:{_palette['primary']} bold"),
        ("question", "bold"),                  # the prompt text
        ("pointer", f"fg:{_palette['primary']} bold"),
        ("highlighted", f"fg:{_palette['primary']} bold"),
        ("selected", f"fg:{_palette['success']}"),
        ("separator", f"fg:{_palette['text-muted']} bold"),
        ("answer", f"fg:{_palette['primary']} bold"),
        ("instruction", f"fg:{_palette['text-muted']}"),
        ("disabled", f"fg:{_palette['text-muted']} italic"),
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
            raise _aborted("select")
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
            raise _aborted("select_grouped")
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
            raise _aborted("multiselect")
        return list(ans)
    pre_idx = [i for i, (v, _l, _h) in enumerate(choices) if v in pre]
    idxs = prompt_checklist(message, _labels(choices), pre_selected=pre_idx)
    return [choices[i][0] for i in idxs]


def text(message: str, default: str = "") -> str:
    if use_rich():
        ans = _q.text(message, default=default, style=_STYLE, qmark=_QMARK).ask()
        if ans is None:
            raise _aborted("text")
        return ans.strip() or default
    return prompt(message, default=default)


def password(message: str) -> str:
    if use_rich():
        ans = _q.password(message, style=_STYLE, qmark=_QMARK).ask()
        if ans is None:
            raise _aborted("password")
        return ans.strip()
    return prompt(message, password=True)


def confirm(message: str, default: bool = True) -> bool:
    if use_rich():
        ans = _q.confirm(message, default=default, style=_STYLE, qmark=_QMARK).ask()
        if ans is None:
            raise _aborted("confirm")
        return bool(ans)
    return prompt_yes_no(message, default=default)


def note(message: str, kind: str = "info") -> None:
    glyph = {"success": "✓", "warning": "!", "error": "✗", "info": "·"}.get(kind, "·")
    role = {"success": "success", "warning": "warning", "error": "error",
            "info": "text-muted"}.get(kind, "text-muted")
    print(color(f"  {glyph} {message}", ansi(role)))


def intro(title: str) -> None:
    print()
    print(color(f"  ❯ {title}", Colors.BOLD, ansi("primary")))


def outro(message: str) -> None:
    print()
    print(color(f"  ● {message}", Colors.BOLD, ansi("success")))
    print()


_SPINNER_FRAMES = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"
_SPINNER_INTERVAL = 0.08


@contextmanager
def spinner(message: str) -> Iterator[None]:
    """Show a live spinner for the duration of the block.

    On a TTY a daemon thread repaints a braille frame in place, so a long
    ``list_models`` / ``verify_model`` call visibly progresses instead of
    leaving one dead line on screen. Without a TTY (CI, piped output, no PTY)
    the animation would emit control-character noise, so we print the single
    static line and skip the thread entirely. Never raises: a failed repaint
    must not take down the wizard.
    """
    label = color(f"  ⋯ {message}", ansi("text-muted"))
    if not sys.stdout.isatty():
        print(label)
        yield
        return

    import threading

    stop = threading.Event()

    def _spin() -> None:
        idx = 0
        try:
            while not stop.wait(_SPINNER_INTERVAL):
                frame = _SPINNER_FRAMES[idx % len(_SPINNER_FRAMES)]
                idx += 1
                sys.stdout.write(
                    "\r" + color(f"  {frame} {message}", ansi("text-muted"))
                )
                sys.stdout.flush()
        except Exception:  # pragma: no cover - a broken tty must not propagate
            pass

    sys.stdout.write(label)
    sys.stdout.flush()
    thread = threading.Thread(target=_spin, daemon=True)
    thread.start()
    try:
        yield
    finally:
        stop.set()
        thread.join(timeout=1.0)
        # Clear the animated line so the caller's own output starts clean, then
        # leave the completed step on screen as a static line.
        try:
            sys.stdout.write("\r\033[2K" + label + "\n")
            sys.stdout.flush()
        except Exception:  # pragma: no cover - see above
            pass
