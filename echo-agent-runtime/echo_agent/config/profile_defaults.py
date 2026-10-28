"""Per-profile cognitive defaults.

Applied after YAML/env/override merge but before pydantic validation. Only
fills a default when the key is absent — an explicit user value always wins.

This hook runs *before* ``Config(**data)``, so ``security.profile`` is only
present here when the user (or an entrypoint override, e.g. the gateway
tightening in ``app.py``) set it explicitly. When it is absent we mirror the
pydantic field default — otherwise pydantic would later fill ``personal_cli``
yet its lean cognitive defaults (planning off, retrieval degrade) would never
apply to the most common zero-config CLI run.
"""
from __future__ import annotations

from typing import Any

from echo_agent.config.schema import SecurityConfig

# Single source of truth for the default profile: the schema field default
# that pydantic injects after this hook. Mirrored here so a config with no
# explicit profile resolves to the same cognitive defaults the effective
# config will end up using.
_DEFAULT_PROFILE: str = SecurityConfig.model_fields["profile"].default

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
    # Absent profile → mirror the pydantic field default so zero-config runs
    # (the common CLI case) get the lean cognitive defaults. An explicit but
    # unknown profile string is left as-is: it injects nothing here and
    # pydantic surfaces the validation error.
    if not profile:
        profile = _DEFAULT_PROFILE
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
