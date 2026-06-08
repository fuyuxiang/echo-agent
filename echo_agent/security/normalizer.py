"""Command normalization — decode obfuscation before security pattern matching.

Handles percent-encoding, path traversal, home expansion, and quote/escape stripping
so that regex-based guards cannot be bypassed with simple encoding tricks.
"""

from __future__ import annotations

import os
import re
from urllib.parse import unquote


_PERCENT_ENCODED_RE = re.compile(r"%[0-9A-Fa-f]{2}")
_BACKSLASH_ESCAPE_RE = re.compile(r"\\(.)")
_CONSECUTIVE_SLASHES_RE = re.compile(r"/{2,}")


def decode_percent_encoding(s: str) -> str:
    if not _PERCENT_ENCODED_RE.search(s):
        return s
    return unquote(s)


def resolve_path_traversal(s: str) -> str:
    """Collapse .. and . segments in any embedded paths."""
    parts = s.split()
    result = []
    for part in parts:
        if "/" in part or part.startswith("."):
            normalized = os.path.normpath(part) if part.startswith("/") or part.startswith(".") else part
            result.append(normalized)
        else:
            result.append(part)
    return " ".join(result)


def expand_home(s: str) -> str:
    parts = s.split()
    result = []
    for part in parts:
        if part.startswith("~/") or part == "~":
            result.append(os.path.expanduser(part))
        else:
            result.append(part)
    return " ".join(result)


def strip_escapes(s: str) -> str:
    """Remove shell escape characters that could hide dangerous commands."""
    return _BACKSLASH_ESCAPE_RE.sub(r"\1", s)


def collapse_slashes(s: str) -> str:
    return _CONSECUTIVE_SLASHES_RE.sub("/", s)


def normalize_command(command: str) -> str:
    """Apply all normalization passes to a shell command string."""
    result = decode_percent_encoding(command)
    result = strip_escapes(result)
    result = resolve_path_traversal(result)
    result = expand_home(result)
    result = collapse_slashes(result)
    return result
