"""Per-profile cognitive defaults.

Applied after YAML/env/override merge but before pydantic validation. Only
fills a default when the key is absent — an explicit user value always wins.
"""
from __future__ import annotations

from typing import Any

# profile -> {section: {field: default}}. "lean" profiles trim the reply path.
_PROFILE_COGNITIVE_DEFAULTS: dict[str, dict[str, dict[str, Any]]] = {
    "personal_cli": {
        "planning": {"enabled": False},
        "memory": {"retrieval_on_miss": "degrade"},
    },
    "daemon": {
        "planning": {"enabled": True},
        "memory": {"retrieval_on_miss": "sync"},
    },
    "public_gateway": {
        "planning": {"enabled": True},
        "memory": {"retrieval_on_miss": "sync"},
    },
}


def apply_profile_cognitive_defaults(data: dict[str, Any]) -> dict[str, Any]:
    profile = ""
    sec = data.get("security")
    if isinstance(sec, dict):
        profile = sec.get("profile", "") or ""
    mapping = _PROFILE_COGNITIVE_DEFAULTS.get(profile)
    if not mapping:
        return data
    for section, fields in mapping.items():
        sub = data.setdefault(section, {})
        if not isinstance(sub, dict):
            continue
        for key, default in fields.items():
            sub.setdefault(key, default)  # explicit user value wins
    return data
