"""Lightweight i18n for the setup wizard.

The wizard supports English and Chinese. Locale is resolved in this order:

1. Explicit override via ``set_locale("zh"|"en")`` (CLI ``--lang``)
2. Saved preference at ``config["ui"]["locale"]``
3. ``LC_ALL`` / ``LC_MESSAGES`` / ``LANG`` env vars
4. ``locale.getlocale()`` fallback
5. macOS ``defaults read -g AppleLocale`` final fallback
"""

from __future__ import annotations

import locale
import os
import subprocess
import sys
from typing import Any

from echo_agent.cli.i18n import en as _en
from echo_agent.cli.i18n import zh as _zh

SUPPORTED = ("en", "zh")
_BUNDLES: dict[str, dict[str, Any]] = {
    "en": _en.MESSAGES,
    "zh": _zh.MESSAGES,
}

_active: str = "en"


def _normalize(raw: str | None) -> str | None:
    if not raw:
        return None
    head = raw.split(".")[0].split("@")[0].strip().lower()
    if not head:
        return None
    if head.startswith("zh"):
        return "zh"
    if head.startswith("en"):
        return "en"
    return None


def _from_macos_defaults() -> str | None:
    if sys.platform != "darwin":
        return None
    try:
        result = subprocess.run(
            ["defaults", "read", "-g", "AppleLocale"],
            capture_output=True, text=True, timeout=2,
        )
    except (OSError, subprocess.SubprocessError):
        return None
    if result.returncode != 0:
        return None
    return _normalize(result.stdout.strip())


def detect_locale(saved: str | None = None) -> str:
    """Resolve the best UI locale for this user."""
    candidate = _normalize(saved)
    if candidate:
        return candidate
    for var in ("LC_ALL", "LC_MESSAGES", "LANG", "LANGUAGE"):
        candidate = _normalize(os.environ.get(var))
        if candidate:
            return candidate
    try:
        sys_loc, _ = locale.getlocale()
    except Exception:
        sys_loc = None
    candidate = _normalize(sys_loc)
    if candidate:
        return candidate
    candidate = _from_macos_defaults()
    if candidate:
        return candidate
    return "en"


def set_locale(code: str) -> str:
    """Set the active locale; returns the normalized code actually used."""
    global _active
    normalized = _normalize(code) or "en"
    _active = normalized if normalized in SUPPORTED else "en"
    return _active


def get_locale() -> str:
    return _active


def t(key: str, **kwargs: Any) -> str:
    """Translate a dotted key into the active locale.

    Falls back to English if the key is missing in the active bundle, and
    finally to the key string itself so missing keys are obvious.
    """
    bundle = _BUNDLES.get(_active, _BUNDLES["en"])
    value = _resolve(bundle, key)
    if value is None and _active != "en":
        value = _resolve(_BUNDLES["en"], key)
    if value is None:
        value = key
    if isinstance(value, str) and kwargs:
        try:
            return value.format(**kwargs)
        except (KeyError, IndexError):
            return value
    return value if isinstance(value, str) else str(value)


def _resolve(bundle: dict[str, Any], key: str) -> Any:
    cur: Any = bundle
    for part in key.split("."):
        if not isinstance(cur, dict) or part not in cur:
            return None
        cur = cur[part]
    return cur


__all__ = ["detect_locale", "set_locale", "get_locale", "t", "SUPPORTED"]
