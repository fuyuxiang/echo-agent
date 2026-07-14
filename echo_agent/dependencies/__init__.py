"""Echo Agent lazy dependency management.

Provides on-demand installation of optional Python packages required by
individual skills. Skills declare their dependencies in SKILL.md frontmatter
(metadata.echo.requires), and this module ensures those packages are available
at runtime — installing them into the active venv with user confirmation when
needed.
"""

from echo_agent.dependencies.lazy_deps import (
    SKILL_DEPS,
    FeatureUnavailable,
    active_features,
    check_all_features,
    ensure,
    ensure_async,
    feature_install_command,
    feature_missing,
    feature_specs,
    install_authorized,
    install_authorized_async,
    is_available,
    refresh_active_features,
)
from echo_agent.dependencies.skill_require import require, require_any

__all__ = [
    "SKILL_DEPS",
    "FeatureUnavailable",
    "active_features",
    "check_all_features",
    "ensure",
    "ensure_async",
    "install_authorized",
    "install_authorized_async",
    "feature_install_command",
    "feature_missing",
    "feature_specs",
    "is_available",
    "refresh_active_features",
    "require",
    "require_any",
]
