"""Tests for ModelRouter health tracking, cooldown, and persistence."""

from __future__ import annotations

import json
from datetime import datetime, timedelta, timezone
from pathlib import Path


from echo_agent.config.schema import ModelsConfig, ProviderConfig
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
    # Recovery transitions to HALF_OPEN with a bounded probe allowance rather
    # than flooding the possibly-still-down provider with all traffic.
    assert router._health["primary"].refresh_if_recovered() is True
    health = router._health["primary"]
    assert health.status == HealthStatus.HALF_OPEN
    assert health.half_open_allowance == health.HALF_OPEN_MAX_PROBES

    # Probe tickets are limited
    assert router._provider_available("primary") is True
    assert router._provider_available("primary") is True
    assert router._provider_available("primary") is False

    # A successful probe promotes to HEALTHY
    router.mark_success("primary")
    assert health.status == HealthStatus.HEALTHY


def test_half_open_probe_failure_returns_to_cooldown() -> None:
    router, _, _ = _make_router(cooldown_seconds=1)

    for i in range(3):
        router.mark_failure("primary", f"e{i}")
    router._health["primary"].cooldown_until = datetime.now(timezone.utc) - timedelta(seconds=1)
    assert router._health["primary"].refresh_if_recovered() is True
    assert router._health["primary"].status == HealthStatus.HALF_OPEN

    # One failed probe is enough to re-enter cooldown — no need for 3 fresh failures.
    router.mark_failure("primary", "probe failed")
    assert router._health["primary"].status == HealthStatus.COOLDOWN
    assert router._health["primary"].cooldown_until is not None


def test_route_candidates_skips_cooldown_provider() -> None:
    router, _, _ = _make_router()

    router.mark_failure("primary", "e1")
    router.mark_failure("primary", "e2")
    router.mark_failure("primary", "e3")

    candidates = router.route_candidates()
    provider_names = [name for name, _, _ in candidates]
    assert "primary" not in provider_names
    assert "backup" in provider_names


def test_health_persistence_save_and_load(tmp_path: Path) -> None:
    health_file = tmp_path / "health.json"
    config = ModelsConfig(
        default_model="fake-model",
        providers=[ProviderConfig(name="primary", api_key="k1")],
        routes=[],
    )
    router = ModelRouter(config, cooldown_seconds=120, health_file=health_file)
    router.register_provider("primary", _FakeProvider())

    router.mark_failure("primary", "e1")
    router.mark_failure("primary", "e2")
    router.mark_failure("primary", "e3")

    assert health_file.exists()
    data = json.loads(health_file.read_text())
    assert data["primary"]["status"] == "cooldown"
    assert data["primary"]["failure_count"] == 3

    router2 = ModelRouter(config, cooldown_seconds=120, health_file=health_file)
    router2.register_provider("primary", _FakeProvider())
    assert router2._health["primary"].status == HealthStatus.COOLDOWN
    assert router2._health["primary"].failure_count == 3


def test_health_persistence_expired_cooldown_resets_on_load(tmp_path: Path) -> None:
    health_file = tmp_path / "health.json"
    expired = (datetime.now(timezone.utc) - timedelta(seconds=300)).isoformat()
    data = {"primary": {"status": "cooldown", "failure_count": 3, "last_error": "e", "cooldown_until": expired}}
    health_file.write_text(json.dumps(data))

    config = ModelsConfig(
        default_model="fake-model",
        providers=[ProviderConfig(name="primary", api_key="k1")],
        routes=[],
    )
    router = ModelRouter(config, cooldown_seconds=120, health_file=health_file)
    assert router._health["primary"].status == HealthStatus.HEALTHY

