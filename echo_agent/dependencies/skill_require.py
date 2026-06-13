"""Convenience helper for skill scripts to ensure their dependencies.

Usage in any skill script:

    # At the top of the script, before importing optional packages:
    from echo_agent.dependencies.skill_require import require
    require("skill.excel-author")

    # Now safe to import:
    from openpyxl import Workbook

This replaces the old try/except ImportError pattern with a single call
that handles detection, user prompting, installation, and clear error
messages.

Behavior varies by environment:
- TTY (CLI): prompt user to confirm install, sys.exit on failure
- Non-TTY (channel/daemon): auto-install if allowed by config; on failure,
  print a user-friendly message to stdout (so the agent can relay it to
  the channel user) and exit with code 0 to avoid crashing the skill runner
"""

from __future__ import annotations

import sys


def _is_interactive() -> bool:
    """Detect whether we're running in an interactive terminal."""
    return sys.stdin.isatty() and sys.stdout.isatty()


def _channel_friendly_exit(feature: str, detail: str) -> None:
    """Output a user-friendly message suitable for relay to channel users.

    Prints to stdout (not stderr) so the agent framework captures it as
    skill output and can forward it to WeChat/Telegram/etc. Exits with
    code 1 but the message is what matters — the agent should relay it.
    """
    from echo_agent.dependencies.lazy_deps import feature_install_command

    install_cmd = ""
    try:
        install_cmd = feature_install_command(feature) or ""
    except Exception:
        pass

    msg_parts = [
        f"This feature ({feature}) requires additional packages that are not yet installed.",
        f"Reason: {detail}",
    ]
    if install_cmd:
        msg_parts.append(f"Administrator can run: {install_cmd}")
    msg_parts.append("Or enable auto-install: set skills.allow_lazy_installs: true in config.")

    print("\n".join(msg_parts))
    sys.exit(1)


def require(feature: str, *, prompt: bool | None = None) -> None:
    """Ensure a skill's dependencies are installed.

    Args:
        feature: Key in SKILL_DEPS (e.g., "skill.excel-author")
        prompt: Whether to ask user confirmation. If None (default),
                auto-detects: True for TTY, False for non-TTY (channels).
    """
    if prompt is None:
        prompt = _is_interactive()

    try:
        from echo_agent.dependencies.lazy_deps import ensure
        ensure(feature, prompt=prompt)
    except ImportError:
        _fallback_check(feature)
    except Exception as e:
        error_msg = str(e)
        if _is_interactive():
            sys.exit(f"[echo-agent] Dependency error: {error_msg}")
        else:
            _channel_friendly_exit(feature, error_msg)


def _fallback_check(feature: str) -> None:
    """Fallback when echo_agent is not importable (standalone script execution).

    Attempts a direct importlib.metadata check and gives a useful error
    message listing what to install.
    """
    try:
        from echo_agent.dependencies.lazy_deps import SKILL_DEPS, _is_satisfied
        specs = SKILL_DEPS.get(feature, ())
        missing = [s for s in specs if not _is_satisfied(s)]
        if missing:
            detail = f"Missing: {', '.join(missing)}"
            if _is_interactive():
                sys.exit(
                    f"[echo-agent] {detail}\n"
                    f"Install: pip install {' '.join(missing)}"
                )
            else:
                _channel_friendly_exit(feature, detail)
    except ImportError:
        pass


def require_any(*packages: str) -> None:
    """Lightweight check that at least one of the listed packages is importable.

    For scripts that only need a presence check without the full lazy-install
    machinery. Falls back gracefully if echo_agent is not on the path.

    Usage:
        require_any("pymupdf", "PyMuPDF")
    """
    from importlib.util import find_spec
    for pkg in packages:
        normalized = pkg.replace("-", "_").split("[")[0]
        if find_spec(normalized):
            return

    detail = f"At least one required: {', '.join(packages)}"
    if _is_interactive():
        sys.exit(
            f"[echo-agent] {detail}\n"
            f"Install: pip install {packages[0]}"
        )
    else:
        print(
            f"This feature requires one of: {', '.join(packages)}\n"
            f"Administrator can run: pip install {packages[0]}"
        )
        sys.exit(1)
