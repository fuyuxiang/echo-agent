"""Command normalization — decode obfuscation before security pattern matching.

Handles percent-encoding, path traversal, home expansion, quote/escape stripping,
ANSI-C quoting ($'...'), and shell variable indirection so that regex-based guards
cannot be bypassed with encoding tricks.
"""

from __future__ import annotations

import os
import re
from fnmatch import fnmatchcase
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
_IFS_DEFAULT = " \t\n"
_IFS_SIMPLE_RE = re.compile(r"\$IFS(?![A-Za-z_0-9])")
_IFS_SLICE_RE = re.compile(r":(\d+)(?::(\d+))?")


def _ifs_slice_index(digits: str) -> int:
    """Clamp untrusted shell offsets without parsing an unbounded integer."""
    significant = digits.lstrip("0") or "0"
    if len(significant) > 1:
        return len(_IFS_DEFAULT) + 1
    return min(int(significant), len(_IFS_DEFAULT) + 1)


def _remove_ifs_pattern(operator: str, pattern: str) -> str:
    """Apply a simple shell prefix/suffix glob to the default IFS value."""
    if not pattern or "[:" in pattern:
        # Unsupported pattern syntax stays conservative: it may leave a separator.
        return _IFS_DEFAULT

    positions = range(len(_IFS_DEFAULT) + 1)
    if operator.startswith("%"):
        matches = [i for i in positions if fnmatchcase(_IFS_DEFAULT[i:], pattern)]
        cut = (min if operator == "%%" else max)(matches) if matches else len(_IFS_DEFAULT)
        return _IFS_DEFAULT[:cut]

    matches = [i for i in positions if fnmatchcase(_IFS_DEFAULT[:i], pattern)]
    cut = (max if operator == "##" else min)(matches) if matches else 0
    return _IFS_DEFAULT[cut:]


def _expand_ifs_brace(body: str, *, custom_ifs: bool) -> str:
    """Model IFS forms whose result is known under the shell's default IFS."""
    if not body.startswith("IFS"):
        return "${" + body + "}"
    modifier = body[3:]
    if modifier and (modifier[0].isascii() and (modifier[0].isalnum() or modifier[0] == "_")):
        # ${IFS_SUFFIX} names a different variable; the generic pass handles it.
        return "${" + body + "}"
    if modifier.startswith((":+", "+")):
        # With IFS set, + uses its word rather than IFS. Preserve the word: it
        # may contain its own field separator, or only ordinary text.
        word = modifier[2:] if modifier.startswith(":+") else modifier[1:]
        return " " if custom_ifs and word else word
    if not modifier or modifier.startswith((":-", "-", ":=", "=", ":?", "?")):
        return _IFS_DEFAULT
    if slice_match := _IFS_SLICE_RE.fullmatch(modifier):
        if custom_ifs:
            return "" if slice_match.group(2) and not _ifs_slice_index(slice_match.group(2)) else " "
        start = _ifs_slice_index(slice_match.group(1))
        length = slice_match.group(2)
        return _IFS_DEFAULT[start:start + _ifs_slice_index(length)] if length is not None else _IFS_DEFAULT[start:]
    if modifier.startswith(("%%", "%", "##", "#")):
        operator = modifier[:2] if modifier[:2] in {"%%", "##"} else modifier[0]
        if custom_ifs:
            return "" if operator in {"%%", "##"} and modifier[len(operator):] == "*" else " "
        return _remove_ifs_pattern(operator, modifier[len(operator):])
    if modifier[:1] in {":", "/", "^", ",", "@", "["}:
        # Dynamic/unsupported transformations may still produce separators.
        return _IFS_DEFAULT
    return "${" + body + "}"


def _expand_ifs_braces(s: str, *, custom_ifs: bool) -> str:
    """Resolve nested ${...} from the inside out in one scan."""
    frames: list[list[str]] = [[]]
    i = 0
    while i < len(s):
        if s.startswith("${", i):
            frames.append([])
            i += 2
        elif s[i] == "}" and len(frames) > 1:
            body = "".join(frames.pop())
            frames[-1].append(_expand_ifs_brace(body, custom_ifs=custom_ifs))
            i += 1
        else:
            frames[-1].append(s[i])
            i += 1
    while len(frames) > 1:
        # Leave unterminated shell syntax intact for the generic pass.
        frames[-2].append("${" + "".join(frames.pop()))
    return "".join(frames[0])


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


def expand_shell_variables(s: str, *, custom_ifs: bool = False) -> str:
    """Strip ${var} and $var references, leaving just the variable name as a marker.

    This ensures patterns like ${cmd} where cmd=rm are surfaced for guard matching.
    We replace ${VAR} with the literal VAR name so guards can detect suspicious names.

    ``$IFS`` is the exception: its unquoted value normally separates fields.
    Model common parameter operators against the default separator before the
    name-marker pass, without treating other variables such as ``IFS_SUFFIX``
    or the literal word in ``${IFS+word}`` as a separator.
    """
    result = _expand_ifs_braces(s, custom_ifs=custom_ifs)
    result = _IFS_SIMPLE_RE.sub(" ", result)
    result = _SHELL_VAR_BRACE_RE.sub(r"\1", result)
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


def normalize_command(command: str, *, custom_ifs: bool = False) -> str:
    """Apply all normalization passes to a shell command string."""
    result = decode_percent_encoding(command)
    result = decode_ansi_c_quoting(result)
    result = strip_escapes(result)
    result = expand_shell_variables(result, custom_ifs=custom_ifs)
    result = expand_home(result)
    result = resolve_path_traversal(result)
    result = collapse_slashes(result)
    return result
