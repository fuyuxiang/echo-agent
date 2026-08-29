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


_BEARER_RE = re.compile(r"(Bearer\s+)\S+", re.IGNORECASE)
_URL_SECRET_RE = re.compile(
    r"([?&](?:token|key|api_key|apikey|secret|access_token|password)=)([^&\s]+)",
    re.IGNORECASE,
)
_HEADER_FLAG_RE = re.compile(
    r"(-H\s+['\"]?(?:Authorization|X-Api-Key)['\"]?\s*:\s*)\S+",
    re.IGNORECASE,
)
_CLI_SECRET_FLAG_RE = re.compile(
    r"((?:--?)(?:api[-_]?key|token|password|passwd|secret|access[-_]?token)"
    r"(?:=|\s+))([^\s'\"]+)",
    re.IGNORECASE,
)


def mask_sensitive_strings(text: str) -> str:
    """Mask Bearer tokens, URL secret params, and CLI header flags in a string."""
    text = _BEARER_RE.sub(lambda m: m.group(1) + "••••", text)
    text = _URL_SECRET_RE.sub(lambda m: m.group(1) + "••••", text)
    text = _HEADER_FLAG_RE.sub(lambda m: m.group(1) + "••••", text)
    text = _CLI_SECRET_FLAG_RE.sub(lambda m: m.group(1) + "••••", text)
    return text


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
        items = list(value)
        redacted: list = []
        previous_secret_flag = False
        for item in items:
            if previous_secret_flag:
                redacted.append(mask(str(item)))
                previous_secret_flag = False
                continue
            redacted.append(redact_for_export(item, key=key))
            if isinstance(item, str):
                flag = item.lstrip("-").replace("-", "_")
                previous_secret_flag = is_secret_key(flag)
        return redacted
    if isinstance(value, str):
        return mask_sensitive_strings(value)
    if value is None or isinstance(value, (bool, int, float)):
        return value
    return mask_sensitive_strings(str(value))


def _redact_value(key: str, value, *, value_width: int = 60) -> str:
    """Recursively redact a value: mask if the key is secret, otherwise recurse
    into dicts/lists looking for nested secrets."""
    if is_secret_key(key):
        return mask(value)
    if isinstance(value, dict):
        parts = []
        for k, v in value.items():
            parts.append(f"{k}={_redact_value(k, v, value_width=value_width)}")
        shown = "{" + ", ".join(parts) + "}"
        return clip(shown, value_width)
    if isinstance(value, list):
        items = [_redact_value(key, item, value_width=value_width) for item in value]
        shown = "[" + ", ".join(items) + "]"
        return clip(shown, value_width)
    shown = clip(value, value_width)
    return mask_sensitive_strings(shown)


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
