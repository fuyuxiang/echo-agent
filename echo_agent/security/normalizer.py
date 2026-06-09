"""Command normalization — decode obfuscation before security pattern matching.

Handles percent-encoding, path traversal, home expansion, quote/escape stripping,
ANSI-C quoting ($'...'), and shell variable indirection so that regex-based guards
cannot be bypassed with encoding tricks.
"""

from __future__ import annotations

import os
import re
from urllib.parse import unquote


_PERCENT_ENCODED_RE = re.compile(r"%[0-9A-Fa-f]{2}")
_BACKSLASH_ESCAPE_RE = re.compile(r"\\(.)")
_CONSECUTIVE_SLASHES_RE = re.compile(r"/{2,}")
_ANSI_C_QUOTE_RE = re.compile(r"""\$'([^']*)'""")
_ANSI_C_HEX_RE = re.compile(r"\\x([0-9A-Fa-f]{2})")
_ANSI_C_OCT_RE = re.compile(r"\\([0-7]{1,3})")
_ANSI_C_SIMPLE = {
    "\\n": "\n", "\\t": "\t", "\\r": "\r",
    "\\a": "\a", "\\b": "\b", "\\f": "\f",
    "\\\\": "\\", "\\'": "'",
}
_SHELL_VAR_BRACE_RE = re.compile(r"\$\{([A-Za-z_][A-Za-z_0-9]*)(?::?[-=+?][^}]*)?\}")
_SHELL_VAR_SIMPLE_RE = re.compile(r"\$([A-Za-z_][A-Za-z_0-9]*)")


def decode_percent_encoding(s: str) -> str:
    if not _PERCENT_ENCODED_RE.search(s):
        return s
    return unquote(s)


def decode_ansi_c_quoting(s: str) -> str:
    """Decode $'\\xNN' and $'\\NNN' ANSI-C style quoting."""
    def _replace(m: re.Match) -> str:
        inner = m.group(1)
        for esc, char in _ANSI_C_SIMPLE.items():
            inner = inner.replace(esc, char)
        inner = _ANSI_C_HEX_RE.sub(lambda x: chr(int(x.group(1), 16)), inner)
        inner = _ANSI_C_OCT_RE.sub(lambda x: chr(int(x.group(1), 8)), inner)
        return inner

    if "$'" not in s:
        return s
    return _ANSI_C_QUOTE_RE.sub(_replace, s)


def expand_shell_variables(s: str) -> str:
    """Strip ${var} and $var references, leaving just the variable name as a marker.

    This ensures patterns like ${cmd} where cmd=rm are surfaced for guard matching.
    We replace ${VAR} with the literal VAR name so guards can detect suspicious names.
    """
    result = _SHELL_VAR_BRACE_RE.sub(r"\1", s)
    result = _SHELL_VAR_SIMPLE_RE.sub(r"\1", result)
    return result


def resolve_path_traversal(s: str) -> str:
    """Collapse .. and . segments in any embedded paths."""
    parts = s.split()
    result = []
    for part in parts:
        if "/" in part or part.startswith("."):
            result.append(os.path.normpath(part))
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
    result = decode_ansi_c_quoting(result)
    result = strip_escapes(result)
    result = expand_shell_variables(result)
    result = expand_home(result)
    result = resolve_path_traversal(result)
    result = collapse_slashes(result)
    return result
