"""Evolution configuration validation — detect dangerous or contradictory configs.

Checks for risky combinations that could lead to silent quality degradation,
rapid churn, or misconfigured gates.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import TYPE_CHECKING

from loguru import logger

if TYPE_CHECKING:
    from echo_agent.config.schema import EvolutionConfig
    from echo_agent.evolution.types import SkillCandidate
    from echo_agent.skills.store import SkillStore


def validate_candidate_identity(candidate: "SkillCandidate", store: "SkillStore") -> str:
    """Automated candidates cannot rename or shadow another skill via aliases."""
    from echo_agent.skills.store import parse_frontmatter

    if candidate.operation == "create":
        metadata, _ = parse_frontmatter(candidate.proposed_content)
        if (metadata.get("name") or candidate.skill_name) != candidate.skill_name:
            return "candidate frontmatter name must match skill_name"
    elif candidate.operation == "patch":
        original = store.read_skill(candidate.skill_name, include_disabled=True)
        if original is not None:
            before, _ = parse_frontmatter(original)
            after, _ = parse_frontmatter(original.replace(candidate.proposed_patch_old, candidate.proposed_patch_new, 1))
            if before.get("name") != after.get("name"):
                return "candidate patch cannot change a skill's frontmatter name"
    return ""


@dataclass(frozen=True)
class ConfigWarning:
    level: str  # "warning" or "error"
    message: str
    fields: list[str] = field(default_factory=list)


def validate_evolution_config(config: "EvolutionConfig") -> list[ConfigWarning]:
    """Check for dangerous or contradictory configuration combinations."""
    warnings: list[ConfigWarning] = []

    if config.auto_promote and not config.require_strict_improvement and config.regression_threshold > 0.1:
        warnings.append(ConfigWarning(
            level="error",
            message=(
                "auto_promote=True with require_strict_improvement=False and "
                "regression_threshold>0.1 risks silent quality degradation — "
                "candidates can be promoted without proving improvement while "
                "tolerating significant regression"
            ),
            fields=["auto_promote", "require_strict_improvement", "regression_threshold"],
        ))

    if config.auto_promote and config.cooldown_seconds_after_promote < 3600:
        warnings.append(ConfigWarning(
            level="warning",
            message=(
                "auto_promote=True with cooldown < 1 hour may cause rapid "
                "churn — multiple candidates can promote in quick succession"
            ),
            fields=["auto_promote", "cooldown_seconds_after_promote"],
        ))

    if config.auto_promote and config.candidate_review_required:
        warnings.append(ConfigWarning(
            level="warning",
            message=(
                "auto_promote=True and candidate_review_required=True are "
                "contradictory — review_required takes precedence, effectively "
                "disabling auto_promote. Consider setting auto_promote=False "
                "for clarity"
            ),
            fields=["auto_promote", "candidate_review_required"],
        ))

    if config.regression_threshold > 0.2:
        warnings.append(ConfigWarning(
            level="warning",
            message=(
                "regression_threshold > 0.2 allows candidates with up to 20%+ "
                "performance drop to pass — this is unusually permissive"
            ),
            fields=["regression_threshold"],
        ))

    if config.max_candidates_per_run > 10:
        warnings.append(ConfigWarning(
            level="warning",
            message=(
                "max_candidates_per_run > 10 may produce long evaluation times "
                "and resource contention"
            ),
            fields=["max_candidates_per_run"],
        ))

    return warnings


def log_config_warnings(config: "EvolutionConfig") -> list[ConfigWarning]:
    """Validate config and log any warnings/errors. Returns the warnings list."""
    warnings = validate_evolution_config(config)
    for w in warnings:
        if w.level == "error":
            logger.error("Evolution config error: {}", w.message)
        else:
            logger.warning("Evolution config warning: {}", w.message)
    return warnings
