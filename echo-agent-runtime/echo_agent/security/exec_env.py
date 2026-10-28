"""Build subprocess environments without inheriting ambient credentials.

Only ordinary process infrastructure crosses into an exec subprocess. Tools
grant credentials through ``credentials`` and other task-specific values through
``extra``; neither grant is inferred from a variable's name or value.
"""

from __future__ import annotations

import os
import re
from collections.abc import Mapping
from urllib.parse import urlsplit

# Shared with skill scripts. Adding an ambient key here is a security decision:
# DATABASE_URL and PIP_INDEX_URL can carry passwords despite their names.
INFRA_ENV_KEYS: tuple[str, ...] = (
    "PATH", "HOME", "USER", "LOGNAME", "SHELL", "TMPDIR", "TEMP", "TMP",
    "TZ", "LANG", "LC_ALL", "LC_CTYPE", "TERM",
    "SSL_CERT_FILE", "SSL_CERT_DIR", "REQUESTS_CA_BUNDLE", "CURL_CA_BUNDLE",
    "HTTP_PROXY", "HTTPS_PROXY", "NO_PROXY",
    "http_proxy", "https_proxy", "no_proxy",
    "PYTHONHASHSEED", "PYTHONIOENCODING", "PYTHONUTF8", "VIRTUAL_ENV",
    "ECHO_AGENT_DISABLE_LAZY_INSTALLS",
)

_PROXY_KEYS = frozenset({"HTTP_PROXY", "HTTPS_PROXY", "http_proxy", "https_proxy"})
_ENV_NAME = re.compile(r"[A-Za-z_][A-Za-z0-9_]*\Z")


def valid_env_name(name: str) -> bool:
    """Only ordinary POSIX-style variable names may be explicitly forwarded."""
    return bool(_ENV_NAME.fullmatch(name))


def selected_ambient_env(
    names: list[str] | tuple[str, ...], base: Mapping[str, str] | None = None,
) -> dict[str, str]:
    """Copy only the operator-approved ambient variables for an exec tool."""
    source = os.environ if base is None else base
    selected: dict[str, str] = {}
    for name in names:
        if not valid_env_name(name):
            raise ValueError(f"Invalid environment variable name: {name!r}")
        value = source.get(name)
        if value is not None:
            selected[name] = value
    return selected


def _has_proxy_credentials(value: str) -> bool:
    """A proxy URL with userinfo is a credential, not ambient infrastructure."""
    if "@" in value:
        # Also covers scheme-relative and malformed proxy URLs that urlsplit
        # would otherwise interpret as a path instead of an authority.
        return True
    try:
        parsed = urlsplit(value if "://" in value else f"http://{value}")
        return parsed.username is not None or parsed.password is not None
    except ValueError:
        # A malformed proxy cannot be safely classified. The caller may pass it
        # explicitly if that exact value is needed for a particular command.
        return True


def safe_base_env(base: Mapping[str, str] | None = None) -> dict[str, str]:
    """Copy the approved ambient infrastructure keys from ``base``."""
    source = os.environ if base is None else base
    env: dict[str, str] = {}
    for key in INFRA_ENV_KEYS:
        value = source.get(key)
        if value is None or (key in _PROXY_KEYS and _has_proxy_credentials(value)):
            continue
        env[key] = value
    return env


def build_exec_env(
    base: Mapping[str, str] | None = None,
    *,
    credentials: Mapping[str, str] | None = None,
    extra: Mapping[str, str] | None = None,
) -> dict[str, str]:
    """Apply explicit per-call grants on top of safe ambient infrastructure."""
    env = safe_base_env(base)
    if extra:
        env.update(extra)
    # A per-call credential grant is more specific than an operator's ambient
    # allowlist. In particular, a stale ambient token must not shadow the
    # credential selected for this request.
    if credentials:
        env.update(credentials)
    return env
