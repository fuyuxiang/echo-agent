"""Credential redaction for anything the model's parameters get rendered into.

The approval panel and the tool detail view render whatever the model passed,
and that routinely includes credentials — which then sit in the transcript and
get written to disk by /save. Both renderers share this one implementation:
a second copy would drift, and a drifted redactor leaks.
"""

from __future__ import annotations

import re

from echo_agent.cli.render.text import clip

# Parameter names whose value must never be shown verbatim. The approval panel
# and the tool detail view render whatever the model passed, and that routinely
# includes credentials — which then sit in the transcript and get written to disk
# by /save. Matched as a substring on the lowercased key, so "api_key",
# "authorization" and "DB_PASSWORD" are all covered.
_SECRET_KEY_HINTS = (
    "password", "passwd", "secret", "token", "api_key", "apikey",
    "credential", "authorization", "auth_header", "private_key", "access_key",
)


def is_secret_key(key: str) -> bool:
    lowered = str(key).lower()
    return any(hint in lowered for hint in _SECRET_KEY_HINTS)


def mask(value: str) -> str:
    """Replace a secret value with a length-preserving placeholder, keeping the
    last 4 characters so the user can still tell two credentials apart."""
    text = str(value)
    if len(text) <= 4:
        return "••••"
    return "••••" + text[-4:]


_BEARER_RE = re.compile(r"(Bearer\s+)[^\s'\"]+", re.IGNORECASE)
_URL_SECRET_RE = re.compile(
    r"([?&](?:token|key|api_key|apikey|secret|access_token|password)=)"
    r"([^&\s'\"]+)",
    re.IGNORECASE,
)
# Shell header flags need their own quoted form.  A generic ``\S+`` redactor
# only hid the word ``Basic`` in ``-H 'Authorization: Basic <credential>'`` and
# left the actual credential visible.  Match the complete quoted header value
# first, then let the narrower standalone patterns below cover log/header text.
_QUOTED_SECRET_HEADER_RE = re.compile(
    r"(?P<flag>(?:-H|--header)\s+)"
    r"(?P<quote>['\"])"
    r"(?P<name>(?:Proxy-)?Authorization|X-Api-Key)\s*:\s*"
    r"(?P<value>(?:\\.|(?!(?P=quote)).)*)"
    r"(?P=quote)",
    re.IGNORECASE,
)
_AUTH_SCHEME_RE = re.compile(
    r"((?:Proxy-)?Authorization\s*:\s*(?:Bearer|Basic)\s+)"
    r"[^\s'\"]+",
    re.IGNORECASE,
)
_AUTH_VALUE_RE = re.compile(
    r"((?:Proxy-)?Authorization\s*:\s*)"
    r"(?!(?:Bearer|Basic|Digest)\b)[^\s'\"]+",
    re.IGNORECASE,
)
_AUTH_DIGEST_RE = re.compile(
    # Quoted shell headers have already been reduced by
    # _QUOTED_SECRET_HEADER_RE.  This fallback covers raw header logs and
    # malformed/unquoted command strings; masking to the line boundary is
    # intentionally conservative because Digest credentials contain spaces.
    # The negative look-behind avoids re-consuming the safe replacement inside
    # a quoted ``-H 'Authorization: ...'`` command.  In an unquoted/raw header,
    # stop at an obvious following URL or at the line boundary.
    r"(?<!['\"])((?:Proxy-)?Authorization\s*:\s*Digest\s+).+?"
    r"(?=\s+https?://|\r?$)",
    re.IGNORECASE | re.MULTILINE,
)
_API_KEY_HEADER_RE = re.compile(
    r"(X-Api-Key\s*:\s*)[^\s'\"]+",
    re.IGNORECASE,
)
_CLI_SECRET_FLAG_RE = re.compile(
    r"((?:--?)(?:api[-_]?key|token|password|passwd|secret|access[-_]?token)"
    r"(?:=|\s+))([^\s'\"]+)",
    re.IGNORECASE,
)
_QUOTED_CLI_SECRET_FLAG_RE = re.compile(
    r"(?P<prefix>(?:--?)(?:api[-_]?key|token|password|passwd|secret|access[-_]?token)"
    r"(?:=|\s+))"
    r"(?P<quote>['\"])"
    r"(?:\\.|(?!(?P=quote)).)*"
    r"(?P=quote)",
    re.IGNORECASE,
)


def _mask_quoted_header(match: re.Match[str]) -> str:
    name = match.group("name")
    value = match.group("value").strip()
    replacement = "••••"
    if "authorization" in name.lower():
        scheme, separator, _credential = value.partition(" ")
        if separator and scheme.lower() in {"bearer", "basic", "digest"}:
            replacement = f"{scheme} ••••"
    quote = match.group("quote")
    return f"{match.group('flag')}{quote}{name}: {replacement}{quote}"


def _mask_quoted_cli_flag(match: re.Match[str]) -> str:
    quote = match.group("quote")
    return f"{match.group('prefix')}{quote}••••{quote}"


def mask_sensitive_strings(text: str) -> str:
    """Mask credentials embedded in free-form command/header text."""
    text = _QUOTED_SECRET_HEADER_RE.sub(_mask_quoted_header, text)
    text = _BEARER_RE.sub(lambda m: m.group(1) + "••••", text)
    text = _AUTH_SCHEME_RE.sub(lambda m: m.group(1) + "••••", text)
    text = _AUTH_VALUE_RE.sub(lambda m: m.group(1) + "••••", text)
    text = _AUTH_DIGEST_RE.sub(lambda m: m.group(1) + "••••", text)
    text = _API_KEY_HEADER_RE.sub(lambda m: m.group(1) + "••••", text)
    text = _URL_SECRET_RE.sub(lambda m: m.group(1) + "••••", text)
    text = _QUOTED_CLI_SECRET_FLAG_RE.sub(_mask_quoted_cli_flag, text)
    text = _CLI_SECRET_FLAG_RE.sub(lambda m: m.group(1) + "••••", text)
    return text


def _standalone_secret_flag(value: str) -> bool:
    """Whether an argv item consumes the following item as its secret value.

    ``--token=value`` already contains its value and therefore must *not* cause
    the next argv item (often the destination URL) to be masked as well.
    """
    item = str(value).strip()
    if not item.startswith("-") or "=" in item:
        return False
    name = item.lstrip("-").replace("-", "_")
    return bool(name) and is_secret_key(name)


def _redact_sequence(values, *, key: str = "") -> list:
    redacted: list = []
    mask_next = False
    for item in values:
        if mask_next:
            redacted.append(mask(str(item)))
            mask_next = False
            continue
        redacted.append(redact_for_export(item, key=key))
        if isinstance(item, str):
            mask_next = _standalone_secret_flag(item)
    return redacted


def redact_for_export(value, *, key: str = ""):
    """Return a JSON-serialisable, recursively redacted copy of *value*.

    Audit exports need the original structure (unlike ``format_params``, which
    intentionally flattens values for a narrow terminal row), but must never
    turn a tool call into a credential dump.  Keep ordinary scalar types, mask
    values under secret-looking keys, and also scrub bearer tokens / credential
    query parameters embedded in otherwise innocent strings.
    """
    if is_secret_key(key):
        return mask(str(value))
    if isinstance(value, dict):
        return {
            str(k): redact_for_export(v, key=str(k))
            for k, v in value.items()
        }
    if isinstance(value, (list, tuple, set)):
        return _redact_sequence(list(value), key=key)
    if isinstance(value, str):
        return mask_sensitive_strings(value)
    if value is None or isinstance(value, (bool, int, float)):
        return value
    return mask_sensitive_strings(str(value))


def _render_redacted(value) -> str:
    if isinstance(value, dict):
        return "{" + ", ".join(
            f"{key}={_render_redacted(item)}" for key, item in value.items()
        ) + "}"
    if isinstance(value, list):
        return "[" + ", ".join(_render_redacted(item) for item in value) + "]"
    return str(value)


def _redact_value(key: str, value, *, value_width: int = 60) -> str:
    """Recursively redact a value before flattening it for terminal display."""
    safe = redact_for_export(value, key=key)
    return clip(_render_redacted(safe), value_width)


def format_params(params: dict, *, value_width: int = 60) -> list[str]:
    """Render call parameters as one ``key=value`` line per entry, with secrets
    masked recursively.

    Both the approval panel and the tool detail view used to print ``str(dict)``,
    i.e. a raw Python repr: it wrapped unreadably for anything non-trivial and
    leaked credentials verbatim on the exact screen where the user is asked to
    authorize a high-risk action.
    """
    lines: list[str] = []
    for key, value in (params or {}).items():
        shown = _redact_value(key, value, value_width=value_width)
        lines.append(f"{key}={shown}")
    return lines
