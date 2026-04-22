"""Model router — routes tasks to appropriate models with fallback, cost control, and health tracking."""

from __future__ import annotations

from dataclasses import dataclass, field
from datetime import datetime, timedelta, timezone
from typing import Any

from loguru import logger

from echo_agent.config.schema import ModelsConfig, ModelRouteConfig
from echo_agent.models.provider import LLMProvider


@dataclass
class RouteDecision:
    provider_name: str = ""
    model: str = ""
    fallback_chain: list[str] = field(default_factory=list)
    reason: str = ""
    context_window: int = 65536
    max_tokens: int = 4096
    temperature: float = 0.7


@dataclass
class ProviderHealth:
    status: str = "healthy"  # healthy | degraded | cooldown | disabled
    failure_count: int = 0
    last_error: str = ""
    cooldown_until: datetime | None = None

    @property
    def is_available(self) -> bool:
        if self.status == "disabled":
            return False
        if self.status == "cooldown" and self.cooldown_until:
            if datetime.now(timezone.utc) < self.cooldown_until:
                return False
            self.status = "healthy"
            self.failure_count = 0
        return True

    @property
    def score(self) -> float:
        scores = {"healthy": 1.0, "degraded": 0.5, "cooldown": 0.0, "disabled": -1.0}
        return scores.get(self.status, 0.0)


class ModelRouter:
    """Routes requests to the best model based on task type, cost, and availability."""

    def __init__(self, config: ModelsConfig, cooldown_seconds: int = 120):
        self._config = config
        self._providers: dict[str, LLMProvider] = {}
        self._daily_cost: float = 0.0
        self._health: dict[str, ProviderHealth] = {}
        self._cooldown_seconds = cooldown_seconds

    def register_provider(self, name: str, provider: LLMProvider) -> None:
        self._providers[name] = provider
        self._health[name] = ProviderHealth()

    def get_provider(self, name: str) -> LLMProvider | None:
        return self._providers.get(name)

    def route(self, task_type: str = "", content: str = "", preferred_model: str = "") -> RouteDecision:
        if preferred_model:
            for route in self._config.routes:
                if route.model == preferred_model:
                    return self._build_decision(route)

        for route in self._config.routes:
            if self._matches_task(route, task_type):
                return self._build_decision(route)

        return RouteDecision(
            model=self._config.default_model,
            reason="default model",
            max_tokens=4096,
        )

    def route_with_fallback(self, task_type: str = "", content: str = "") -> tuple[LLMProvider, RouteDecision]:
        decision = self.route(task_type, content)
        provider = self._find_healthy_provider(decision.model)
        if provider:
            return provider, decision

        for fallback_model in decision.fallback_chain:
            provider = self._find_healthy_provider(fallback_model)
            if provider:
                decision.model = fallback_model
                decision.reason = f"fallback from {decision.model}"
                return provider, decision

        first_provider = next(iter(self._providers.values()), None)
        if first_provider:
            return first_provider, decision
        raise RuntimeError("No LLM providers available")

    def mark_failure(self, provider_name: str, error: str = "") -> None:
        health = self._health.get(provider_name)
        if not health:
            health = ProviderHealth()
            self._health[provider_name] = health
        health.failure_count += 1
        health.last_error = error
        if health.failure_count >= 3:
            health.status = "cooldown"
            health.cooldown_until = datetime.now(timezone.utc) + timedelta(seconds=self._cooldown_seconds)
            logger.warning("Provider {} -> cooldown (failures={})", provider_name, health.failure_count)
        else:
            health.status = "degraded"
            logger.info("Provider {} -> degraded (failures={})", provider_name, health.failure_count)

    def mark_success(self, provider_name: str) -> None:
        health = self._health.get(provider_name)
        if health and health.status != "healthy":
            health.status = "healthy"
            health.failure_count = 0
            health.cooldown_until = None
            logger.info("Provider {} -> healthy", provider_name)

    def mark_unhealthy(self, provider_name: str) -> None:
        self.mark_failure(provider_name, "manual")

    def mark_healthy(self, provider_name: str) -> None:
        self.mark_success(provider_name)

    def get_health_summary(self) -> dict[str, dict[str, Any]]:
        return {
            name: {"status": h.status, "failures": h.failure_count, "score": h.score}
            for name, h in self._health.items()
        }

    def check_cost_limit(self) -> bool:
        if self._config.cost_limit_daily_usd <= 0:
            return True
        return self._daily_cost < self._config.cost_limit_daily_usd

    def record_cost(self, amount: float) -> None:
        self._daily_cost += amount

    def reset_daily_cost(self) -> None:
        self._daily_cost = 0.0

    def _build_decision(self, route: ModelRouteConfig) -> RouteDecision:
        return RouteDecision(
            provider_name=route.provider,
            model=route.model,
            fallback_chain=route.fallback_models,
            context_window=route.context_window,
            max_tokens=route.max_tokens,
            temperature=route.temperature,
            reason="route match",
        )

    def _matches_task(self, route: ModelRouteConfig, task_type: str) -> bool:
        if not task_type:
            return False
        if not route.provider:
            return False
        provider_lower = route.provider.lower()
        task_lower = task_type.lower()
        if task_lower == provider_lower:
            return True
        if task_lower in route.model.lower():
            return True
        return False

    def _find_healthy_provider(self, model: str) -> LLMProvider | None:
        for name, provider in self._providers.items():
            health = self._health.get(name)
            if health and not health.is_available:
                continue
            if hasattr(provider, "get_default_model"):
                default = provider.get_default_model()
                if model and default and model.lower().startswith(default.split("/")[0].lower()):
                    return provider
            if hasattr(provider, "api_base") or hasattr(provider, "api_key"):
                return provider
        return next(iter(self._providers.values()), None)
