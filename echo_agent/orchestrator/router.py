"""Route planner — task classification, model selection, provider health."""

from __future__ import annotations

import re
from datetime import datetime, timezone
from typing import Any

from loguru import logger

from echo_agent.config.schema import AgentProfile, Config
from echo_agent.orchestrator.models import ProviderHealthState, RouteDecision

_TASK_PATTERNS: dict[str, list[str]] = {
    "debugging": ["debug", "error", "traceback", "exception", "fix", "bug", "crash", "fail"],
    "review": ["review", "check", "audit", "inspect", "evaluate"],
    "testing": ["test", "spec", "assert", "coverage", "unittest", "pytest"],
    "refactor": ["refactor", "clean", "reorganize", "restructure", "simplify"],
    "coding": ["implement", "create", "build", "add", "write", "develop", "code"],
    "research": ["research", "investigate", "explore", "analyze", "study", "compare"],
    "search": ["search", "find", "look for", "grep", "locate"],
    "writing": ["write", "draft", "compose", "document", "readme", "blog"],
    "ops": ["deploy", "ci", "cd", "docker", "kubernetes", "infra", "terraform"],
    "scheduling": ["schedule", "cron", "remind", "timer", "recurring"],
    "communication": ["send", "notify", "message", "email", "slack", "telegram"],
}

_HEALTH_SCORES = {"healthy": 1.0, "degraded": 0.5, "cooldown": 0.0, "disabled": -1.0}


def classify_provider_error(error: Exception) -> str:
    msg = str(error).lower()
    if "timeout" in msg:
        return "timeout"
    if "rate" in msg or "429" in msg:
        return "rate_limit"
    if "quota" in msg or "billing" in msg:
        return "quota"
    if "401" in msg or "403" in msg or "auth" in msg:
        return "configuration"
    if "ssl" in msg or "tls" in msg:
        return "tls"
    if "connect" in msg or "refused" in msg:
        return "connection"
    if "500" in msg or "502" in msg or "503" in msg:
        return "server_error"
    return "unknown"


class RoutePlanner:
    """Classifies tasks, selects models, and tracks provider health."""

    def __init__(self, config: Config, health: dict[str, ProviderHealthState] | None = None):
        self._config = config
        self._health: dict[str, ProviderHealthState] = health or {}
        self._load: dict[str, int] = {}

    def classify(self, content: str, media: list[str] | None = None) -> str:
        text = content.lower()
        if media:
            return "multimodal"
        best_kind = "coding"
        best_score = 0
        for kind, patterns in _TASK_PATTERNS.items():
            score = sum(1 for p in patterns if p in text)
            if score > best_score:
                best_score = score
                best_kind = kind
        return best_kind

    def choose_executors(self, task_type: str, content: str) -> list[AgentProfile]:
        executors = self._config.orchestration.executors
        if not executors:
            return [self._config.orchestration.coordinator]
        matched = []
        for eid, profile in executors.items():
            if profile.role and task_type in profile.role:
                matched.append(profile)
        if not matched:
            return [self._config.orchestration.coordinator]
        return matched[:3]

    def choose_model(self, profile: AgentProfile, task_type: str, content: str) -> RouteDecision:
        model = profile.model or self._config.models.default_model
        provider = self._find_provider_for_model(model)
        fallback_chain = self._build_fallback_chain(model)
        health = self._get_health(provider)
        return RouteDecision(
            provider=provider, model=model,
            reason=f"task={task_type} profile={profile.id}",
            route_kind="primary",
            fallback_chain=fallback_chain,
            health_score=_HEALTH_SCORES.get(health.status, 1.0),
        )

    def next_fallback_route(self, current: RouteDecision) -> RouteDecision | None:
        chain = current.fallback_chain
        idx = current.attempt_index + 1
        max_attempts = self._config.orchestration.routing.max_provider_fallback_attempts
        if idx >= max_attempts or idx >= len(chain):
            return None
        next_model = chain[idx]
        provider = self._find_provider_for_model(next_model)
        health = self._get_health(provider)
        if health.status in ("cooldown", "disabled"):
            if idx + 1 < len(chain):
                return current.derive_child(
                    self._find_provider_for_model(chain[idx + 1]),
                    chain[idx + 1],
                    f"skip {provider} ({health.status})",
                )
            return None
        return current.derive_child(provider, next_model, f"fallback attempt {idx}")

    def mark_failure(self, provider: str, error: Exception) -> ProviderHealthState:
        error_kind = classify_provider_error(error)
        current = self._get_health(provider)
        failure_count = current.failure_count + 1
        now = datetime.now(timezone.utc).isoformat()

        if error_kind in ("quota", "rate_limit"):
            cooldown_secs = self._config.orchestration.routing.provider_cooldown_seconds
            from datetime import timedelta
            cooldown_until = (datetime.now(timezone.utc) + timedelta(seconds=cooldown_secs)).isoformat()
            state = ProviderHealthState(
                provider=provider, status="cooldown", cooldown_until=cooldown_until,
                last_error_kind=error_kind, failure_count=failure_count, updated_at=now,
            )
        elif error_kind == "configuration":
            state = ProviderHealthState(
                provider=provider, status="disabled",
                last_error_kind=error_kind, failure_count=failure_count, updated_at=now,
            )
        elif failure_count >= 3:
            from datetime import timedelta
            cooldown_secs = self._config.orchestration.routing.provider_cooldown_seconds
            cooldown_until = (datetime.now(timezone.utc) + timedelta(seconds=cooldown_secs)).isoformat()
            state = ProviderHealthState(
                provider=provider, status="cooldown", cooldown_until=cooldown_until,
                last_error_kind=error_kind, failure_count=failure_count, updated_at=now,
            )
        else:
            state = ProviderHealthState(
                provider=provider, status="degraded",
                last_error_kind=error_kind, failure_count=failure_count, updated_at=now,
            )
        self._health[provider] = state
        logger.info("Provider {} -> {} (failures={}, error={})", provider, state.status, failure_count, error_kind)
        return state

    def mark_success(self, provider: str) -> None:
        current = self._get_health(provider)
        if current.status != "healthy":
            self._health[provider] = ProviderHealthState(provider=provider, status="healthy")
            logger.info("Provider {} -> healthy", provider)

    def replace_health(self, health: dict[str, ProviderHealthState]) -> None:
        self._health = health

    def export_health(self) -> dict[str, ProviderHealthState]:
        now = datetime.now(timezone.utc)
        for p, h in list(self._health.items()):
            if h.status == "cooldown" and h.cooldown_until:
                try:
                    until = datetime.fromisoformat(h.cooldown_until)
                    if now >= until:
                        self._health[p] = ProviderHealthState(provider=p, status="healthy")
                except ValueError:
                    pass
        return dict(self._health)

    def begin_load(self, agent_id: str) -> None:
        self._load[agent_id] = self._load.get(agent_id, 0) + 1

    def end_load(self, agent_id: str) -> None:
        self._load[agent_id] = max(0, self._load.get(agent_id, 0) - 1)

    def _get_health(self, provider: str) -> ProviderHealthState:
        return self._health.get(provider, ProviderHealthState(provider=provider))

    def _find_provider_for_model(self, model: str) -> str:
        for route in self._config.models.routes:
            if route.model == model and route.provider:
                return route.provider
        for prov in self._config.models.providers:
            if model in prov.models:
                return prov.name
        return "default"

    def _build_fallback_chain(self, primary_model: str) -> list[str]:
        chain = [primary_model]
        for route in self._config.models.routes:
            if route.model == primary_model:
                chain.extend(route.fallback_models)
                break
        if self._config.models.fallback_model and self._config.models.fallback_model not in chain:
            chain.append(self._config.models.fallback_model)
        return chain
