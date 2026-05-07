"""Tests for ModelRouter health tracking, cooldown, and cost control."""

from __future__ import annotations

from datetime import datetime, timedelta, timezone
from unittest.mock import MagicMock

import pytest

from echo_agent.config.schema import ModelsConfig, ModelRouteConfig, ProviderConfig
from echo_agent.models.provider import LLMProvider, LLMResponse
from echo_agent.models.router import ModelRouter, HealthStatus


class _FakeProvider(LLMProvider):
    async def chat(self, messages, tools=None, model=None, tool_choice=None, **kwargs):
        return LLMResponse(content="ok", finish_reason="stop")

    def get_default_model(self):
        return "fake-model"


def _make_router(*, cooldown_seconds: int = 2) -> tuple[ModelRouter, _FakeProvider, _FakeProvider]:
    config = ModelsConfig(
        default_model="fake-model",
        fallback_model="backup-model",
        providers=[
            ProviderConfig(name="primary", api_key="k1"),
            ProviderConfig(name="backup", api_key="k2"),
        ],
        routes=[],
    )
    router = ModelRouter(config, cooldown_seconds=cooldown_seconds)
    p1 = _FakeProvider()
    p2 = _FakeProvider()
    router.register_provider("primary", p1)
    router.register_provider("backup", p2)
    return router, p1, p2


def test_mark_failure_enters_cooldown_after_threshold() -> None:
    router, _, _ = _make_router()

    router.mark_failure("primary", "error 1")
    router.mark_failure("primary", "error 2")
    assert router._health["primary"].status == HealthStatus.DEGRADED

    router.mark_failure("primary", "error 3")
    assert router._health["primary"].status == HealthStatus.COOLDOWN
    assert router._health["primary"].cooldown_until is not None


def test_mark_success_resets_health() -> None:
    router, _, _ = _make_router()

    router.mark_failure("primary", "error 1")
    router.mark_failure("primary", "error 2")
    router.mark_success("primary")

    assert router._health["primary"].status == HealthStatus.HEALTHY
    assert router._health["primary"].failure_count == 0


def test_cooldown_recovery_after_timeout() -> None:
    router, _, _ = _make_router(cooldown_seconds=1)

    router.mark_failure("primary", "e1")
    router.mark_failure("primary", "e2")
    router.mark_failure("primary", "e3")
    assert router._health["primary"].status == HealthStatus.COOLDOWN

    # Simulate cooldown expired
    router._health["primary"].cooldown_until = datetime.now(timezone.utc) - timedelta(seconds=1)

    assert router._health["primary"].is_available is True
    assert router._health["primary"].status == HealthStatus.HEALTHY


def test_route_candidates_skips_cooldown_provider() -> None:
    router, _, _ = _make_router()

    router.mark_failure("primary", "e1")
    router.mark_failure("primary", "e2")
    router.mark_failure("primary", "e3")

    candidates = router.route_candidates()
    provider_names = [name for name, _, _ in candidates]
    assert "primary" not in provider_names
    assert "backup" in provider_names


def test_cost_limit_enforcement() -> None:
    router, _, _ = _make_router()
    router._config.cost_limit_daily_usd = 1.0

    router.record_cost(0.8)
    assert router.check_cost_limit() is True

    router.record_cost(0.3)
    assert router.check_cost_limit() is False
