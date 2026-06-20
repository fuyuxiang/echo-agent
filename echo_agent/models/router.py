"""Model router — routes tasks to appropriate models with fallback, cost control, and health tracking."""

from __future__ import annotations

import asyncio
import json
import os
import tempfile
import threading
from dataclasses import dataclass, field
from datetime import datetime, timedelta, timezone
from enum import Enum
from pathlib import Path
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


class HealthStatus(str, Enum):
    """模型提供商健康状态。"""
    HEALTHY = "healthy"
    DEGRADED = "degraded"
    COOLDOWN = "cooldown"
    HALF_OPEN = "half_open"
    DISABLED = "disabled"


@dataclass
class ProviderHealth:
    status: HealthStatus = HealthStatus.HEALTHY
    failure_count: int = 0
    last_error: str = ""
    cooldown_until: datetime | None = None
    # Probe tickets handed out while HALF_OPEN — bounds the thundering herd
    # on a recovering provider to a couple of requests instead of all traffic.
    half_open_allowance: int = 0

    HALF_OPEN_MAX_PROBES = 2

    @property
    def is_available(self) -> bool:
        if self.status == HealthStatus.DISABLED:
            return False
        if self.status == HealthStatus.COOLDOWN and self.cooldown_until:
            return datetime.now(timezone.utc) >= self.cooldown_until
        if self.status == HealthStatus.HALF_OPEN:
            return self.half_open_allowance > 0
        return True

    def refresh_if_recovered(self) -> bool:
        """If the cooldown window has passed, transition to HALF_OPEN.

        The provider then serves a bounded number of probe requests; the first
        ``mark_success`` promotes it to HEALTHY, the first ``mark_failure``
        sends it straight back to COOLDOWN. Returns True iff state was mutated.
        """
        if (
            self.status == HealthStatus.COOLDOWN
            and self.cooldown_until
            and datetime.now(timezone.utc) >= self.cooldown_until
        ):
            self.status = HealthStatus.HALF_OPEN
            self.half_open_allowance = self.HALF_OPEN_MAX_PROBES
            self.cooldown_until = None
            return True
        return False

    def take_probe_ticket(self) -> bool:
        """Consume one HALF_OPEN probe slot. Returns False when exhausted."""
        if self.half_open_allowance > 0:
            self.half_open_allowance -= 1
            return True
        return False

    @property
    def score(self) -> float:
        scores = {
            HealthStatus.HEALTHY: 1.0,
            HealthStatus.DEGRADED: 0.5,
            HealthStatus.HALF_OPEN: 0.25,
            HealthStatus.COOLDOWN: 0.0,
            HealthStatus.DISABLED: -1.0,
        }
        return scores.get(self.status, 0.0)


class ModelRouter:
    """Routes requests to the best model based on task type, cost, and availability."""

    def __init__(self, config: ModelsConfig, cooldown_seconds: int = 120, health_file: Path | None = None):
        self._config = config
        self._providers: dict[str, LLMProvider] = {}
        self._health: dict[str, ProviderHealth] = {}
        self._cooldown_seconds = cooldown_seconds
        self._health_file = health_file
        self._health_save_lock = threading.Lock()
        if health_file:
            self._load_health()

    def register_provider(self, name: str, provider: LLMProvider) -> None:
        self._providers[name] = provider
        if name not in self._health:
            self._health[name] = ProviderHealth()

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

    def route_candidates(
        self,
        task_type: str = "",
        content: str = "",
        preferred_model: str = "",
    ) -> list[tuple[str, LLMProvider, RouteDecision]]:
        """构建模型降级链：优先模型 → 任务路由 → 默认模型 → 可用备选。

        按健康状态和配置优先级排序，返回 (提供商, 路由决策) 列表。
        """
        base = self.route(task_type, content, preferred_model=preferred_model)
        model_chain: list[str] = []
        for model in [base.model, *base.fallback_chain, self._config.fallback_model]:
            if model and model not in model_chain:
                model_chain.append(model)

        candidates: list[tuple[str, LLMProvider, RouteDecision]] = []
        seen: set[tuple[str, str]] = set()
        for index, model in enumerate(model_chain):
            preferred_provider = base.provider_name if index == 0 else ""
            entry = self._find_healthy_provider_entry(model, preferred_provider=preferred_provider)
            if not entry:
                continue
            provider_name, provider = entry
            key = (provider_name, model)
            if key in seen:
                continue
            seen.add(key)
            decision = RouteDecision(
                provider_name=provider_name,
                model=model,
                fallback_chain=model_chain[index + 1:],
                reason=base.reason if index == 0 else f"fallback after {base.model}",
                context_window=base.context_window,
                max_tokens=base.max_tokens,
                temperature=base.temperature,
            )
            candidates.append((provider_name, provider, decision))

        for provider_name, provider in self._providers.items():
            if not self._provider_available(provider_name):
                continue
            default_model = provider.get_default_model() if hasattr(provider, "get_default_model") else ""
            key = (provider_name, default_model)
            if key in seen:
                continue
            seen.add(key)
            candidates.append((provider_name, provider, RouteDecision(
                provider_name=provider_name,
                model=default_model or base.model,
                fallback_chain=[],
                reason="available provider fallback",
                max_tokens=base.max_tokens,
                temperature=base.temperature,
                context_window=base.context_window,
            )))
        return candidates

    def resolve(
        self,
        task_type: str,
        *,
        fallback_provider: LLMProvider | None = None,
        fallback_model: str = "",
    ) -> tuple[LLMProvider | None, str]:
        """Resolve a non-inference subsystem (compression, approval, evolution)
        to a (provider, model) pair by task_type.

        If a configured route matches ``task_type`` and its provider is
        registered + healthy, use it. Otherwise fall back to
        ``fallback_provider``/``fallback_model`` (the subsystem's current
        main-provider behaviour) so an unconfigured deployment is unaffected."""
        for route in self._config.routes:
            if not self._matches_task(route, task_type):
                continue
            entry = self._find_healthy_provider_entry(
                route.model, preferred_provider=route.provider
            )
            if entry:
                _, provider = entry
                return provider, (route.model or fallback_model)
        return fallback_provider, fallback_model

    def find_embed_provider(
        self, preferred_model: str = ""
    ) -> tuple[LLMProvider | None, str]:
        """Find a registered provider that actually implements ``embed``.

        Fixes the silent-None embedding gap: when the main provider does not
        support embeddings (e.g. Anthropic), route embedding to any registered
        provider that overrides ``embed`` (e.g. an OpenAI-compatible one). A
        route tagged with task_type ``embedding`` takes precedence so operators
        can pin the embedding model/provider explicitly."""
        # 1. Explicit embedding route wins.
        for route in self._config.routes:
            if not self._matches_task(route, "embedding"):
                continue
            provider = self._providers.get(route.provider)
            if provider is not None and self._supports_embed(provider):
                return provider, (route.model or preferred_model)
        # 2. Any registered embed-capable provider.
        for name, provider in self._providers.items():
            if self._supports_embed(provider) and self._provider_available(name):
                return provider, preferred_model
        return None, preferred_model

    @staticmethod
    def _supports_embed(provider: LLMProvider) -> bool:
        return (
            hasattr(provider, "embed")
            and type(provider).embed is not LLMProvider.embed
        )

    def mark_failure(self, provider_name: str, error: str = "") -> None:
        health = self._health.get(provider_name)
        if not health:
            health = ProviderHealth()
            self._health[provider_name] = health
        health.failure_count += 1
        health.last_error = error
        if health.status == HealthStatus.HALF_OPEN:
            # A failed probe sends the provider straight back to cooldown.
            health.status = HealthStatus.COOLDOWN
            health.cooldown_until = datetime.now(timezone.utc) + timedelta(seconds=self._cooldown_seconds)
            health.half_open_allowance = 0
            logger.warning("Provider {} probe failed -> cooldown", provider_name)
        elif health.failure_count >= 3:
            health.status = HealthStatus.COOLDOWN
            health.cooldown_until = datetime.now(timezone.utc) + timedelta(seconds=self._cooldown_seconds)
            logger.warning("Provider {} -> cooldown (failures={})", provider_name, health.failure_count)
        else:
            health.status = HealthStatus.DEGRADED
            logger.info("Provider {} -> degraded (failures={})", provider_name, health.failure_count)
        self._schedule_save_health()

    def mark_success(self, provider_name: str) -> None:
        health = self._health.get(provider_name)
        if health and health.status != HealthStatus.HEALTHY:
            health.status = HealthStatus.HEALTHY
            health.failure_count = 0
            health.cooldown_until = None
            health.half_open_allowance = 0
            logger.info("Provider {} -> healthy", provider_name)
            self._schedule_save_health()

    def _build_decision(self, route: ModelRouteConfig) -> RouteDecision:
        return RouteDecision(
            provider_name=route.provider,
            model=route.model,
            fallback_chain=route.fallback_models,
            max_tokens=route.max_tokens,
            temperature=route.temperature,
            reason="route match",
        )

    def _matches_task(self, route: ModelRouteConfig, task_type: str) -> bool:
        if not task_type:
            return False
        if route.task_types and task_type.lower() in {item.lower() for item in route.task_types}:
            return True
        if not route.provider:
            return False
        provider_lower = route.provider.lower()
        task_lower = task_type.lower()
        if task_lower == provider_lower:
            return True
        if task_lower in route.model.lower():
            return True
        return False

    def _find_healthy_provider_entry(
        self,
        model: str,
        *,
        preferred_provider: str = "",
    ) -> tuple[str, LLMProvider] | None:
        if preferred_provider and preferred_provider in self._providers and self._provider_available(preferred_provider):
            return preferred_provider, self._providers[preferred_provider]
        for pc in self._config.providers:
            if not pc.name or pc.name not in self._providers or not self._provider_available(pc.name):
                continue
            if self._provider_config_supports_model(pc.name, model):
                return pc.name, self._providers[pc.name]
        for name, provider in self._providers.items():
            if not self._provider_available(name):
                continue
            default = provider.get_default_model() if hasattr(provider, "get_default_model") else ""
            if not model or not default or model == default or model.lower().startswith(default.split("/")[0].lower()):
                return name, provider
        if not model:
            for name, provider in self._providers.items():
                if self._provider_available(name):
                    return name, provider
        return None

    def _provider_available(self, provider_name: str) -> bool:
        health = self._health.get(provider_name)
        if health is None:
            return provider_name in self._providers
        # Promote out of cooldown when the window has passed before answering.
        if health.refresh_if_recovered():
            self._schedule_save_health()
        if health.status == HealthStatus.HALF_OPEN:
            # Hand out a bounded number of probe tickets instead of opening
            # the floodgates on a provider that may still be down.
            return health.take_probe_ticket()
        return health.is_available

    def _provider_config_supports_model(self, provider_name: str, model: str) -> bool:
        for pc in self._config.providers:
            if pc.name != provider_name:
                continue
            if not model:
                return True
            if not pc.models:
                return True
            return model in pc.models
        return False

    def _schedule_save_health(self) -> None:
        """Persist health state without blocking the event loop.

        The snapshot is built synchronously (health is only mutated on the
        loop); only the fsync'd file write moves to the executor.
        """
        if not self._health_file:
            return
        data = self._health_snapshot()
        try:
            loop = asyncio.get_running_loop()
        except RuntimeError:
            self._write_health(data)
            return
        loop.run_in_executor(None, self._write_health, data)

    def _health_snapshot(self) -> dict[str, Any]:
        data: dict[str, Any] = {}
        for name, h in self._health.items():
            data[name] = {
                "status": h.status.value,
                "failure_count": h.failure_count,
                "last_error": h.last_error,
                "cooldown_until": h.cooldown_until.isoformat() if h.cooldown_until else None,
            }
        return data

    def _save_health(self) -> None:
        if not self._health_file:
            return
        self._write_health(self._health_snapshot())

    def _write_health(self, data: dict[str, Any]) -> None:
        with self._health_save_lock:
            try:
                self._health_file.parent.mkdir(parents=True, exist_ok=True)
                fd, tmp = tempfile.mkstemp(dir=str(self._health_file.parent), suffix=".tmp")
                try:
                    with os.fdopen(fd, "w", encoding="utf-8") as f:
                        json.dump(data, f, indent=2, ensure_ascii=False)
                    os.replace(tmp, str(self._health_file))
                except BaseException:
                    try:
                        os.unlink(tmp)
                    except OSError:
                        pass
                    raise
            except Exception as e:
                logger.debug("Failed to save health state: {}", e)

    def _load_health(self) -> None:
        if not self._health_file or not self._health_file.exists():
            return
        try:
            data = json.loads(self._health_file.read_text(encoding="utf-8"))
        except Exception as e:
            logger.debug("Failed to load health state: {}", e)
            return
        for name, info in data.items():
            cooldown_until = None
            if info.get("cooldown_until"):
                try:
                    cooldown_until = datetime.fromisoformat(info["cooldown_until"])
                except (ValueError, TypeError):
                    pass
            status = HealthStatus(info.get("status", "healthy"))
            if status == HealthStatus.COOLDOWN and cooldown_until:
                if datetime.now(timezone.utc) >= cooldown_until:
                    status = HealthStatus.HEALTHY
                    cooldown_until = None
            self._health[name] = ProviderHealth(
                status=status,
                failure_count=info.get("failure_count", 0),
                last_error=info.get("last_error", ""),
                cooldown_until=cooldown_until,
            )
