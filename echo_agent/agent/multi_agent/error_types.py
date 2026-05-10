"""Error classification for agent tool failures."""

from __future__ import annotations

from enum import Enum


class ToolErrorType(str, Enum):
    CONFIG = "config"
    AUTH = "auth"
    RATE_LIMIT = "rate_limit"
    TRANSIENT = "transient"
    UNKNOWN = "unknown"


_CONFIG_PATTERNS = (
    "not configured", "api key", "api_key", "missing key",
    "no api", "not set", "requires configuration",
)
_AUTH_PATTERNS = ("unauthorized", "forbidden", "401", "403", "invalid key", "expired")
_RATE_PATTERNS = ("rate limit", "too many requests", "429", "throttl", "quota")
_TRANSIENT_PATTERNS = ("timeout", "timed out", "connection", "500", "502", "503", "504", "temporary")


def classify_tool_error(error_text: str) -> ToolErrorType:
    """Classify a tool error string into a category for recovery decisions."""
    lower = error_text.lower()
    if any(p in lower for p in _CONFIG_PATTERNS):
        return ToolErrorType.CONFIG
    if any(p in lower for p in _AUTH_PATTERNS):
        return ToolErrorType.AUTH
    if any(p in lower for p in _RATE_PATTERNS):
        return ToolErrorType.RATE_LIMIT
    if any(p in lower for p in _TRANSIENT_PATTERNS):
        return ToolErrorType.TRANSIENT
    return ToolErrorType.UNKNOWN
