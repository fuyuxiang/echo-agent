"""Positive interval/iteration bounds for long-running runtime loops."""

from __future__ import annotations

import pytest
from pydantic import ValidationError

from echo_agent.config.schema import Config


@pytest.mark.parametrize("value", [0, -1, -5])
def test_agent_max_iterations_rejects_non_positive(value: int) -> None:
    with pytest.raises(ValidationError):
        Config.model_validate({"agent": {"maxIterations": value}})


@pytest.mark.parametrize("value", [0, -1, -5])
def test_multi_agent_max_iterations_rejects_non_positive(value: int) -> None:
    with pytest.raises(ValidationError):
        Config.model_validate({"multiAgent": {"maxIterations": value}})


@pytest.mark.parametrize("value", [0, -1])
def test_worker_profile_max_iterations_rejects_non_positive(value: int) -> None:
    """A profile participates in min(global, profile), so zero would bypass
    the positive global limit and suppress the worker loop entirely."""
    with pytest.raises(ValidationError):
        Config.model_validate({
            "multiAgent": {
                "workerProfiles": [{"id": "reviewer", "maxIterations": value}],
            },
        })


@pytest.mark.parametrize("value", [0, -1])
def test_health_check_interval_rejects_non_positive(value: int) -> None:
    """asyncio.sleep(0) would turn the health checker into a busy loop."""
    with pytest.raises(ValidationError):
        Config.model_validate({
            "observability": {"healthCheckIntervalSeconds": value},
        })


@pytest.mark.parametrize("value", [0, -1])
def test_otel_export_interval_rejects_non_positive(value: int) -> None:
    """PeriodicExportingMetricReader requires a strictly positive interval."""
    with pytest.raises(ValidationError):
        Config.model_validate({
            "observability": {"otelExportIntervalMs": value},
        })


def test_minimum_runtime_loop_values_are_accepted() -> None:
    config = Config.model_validate({
        "agent": {"maxIterations": 1},
        "multiAgent": {
            "maxIterations": 1,
            "workerProfiles": [{"id": "reviewer", "maxIterations": 1}],
        },
        "observability": {
            "healthCheckIntervalSeconds": 1,
            "otelExportIntervalMs": 1,
        },
    })

    assert config.agent.max_iterations == 1
    assert config.multi_agent.max_iterations == 1
    assert config.multi_agent.worker_profiles[0].max_iterations == 1
    assert config.observability.health_check_interval_seconds == 1
    assert config.observability.otel_export_interval_ms == 1
