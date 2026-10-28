"""Failure-path and lifecycle tests for the observability layer.

These tests intentionally exercise the enabled OpenTelemetry path with fakes.
The base test environment does not install the optional ``otel`` extra, and the
old suite consequently proved only that every helper was a no-op.
"""

from __future__ import annotations

import asyncio
from unittest.mock import MagicMock

import pytest


def _install_fake_otel(monkeypatch: pytest.MonkeyPatch):
    from echo_agent.observability import telemetry

    tracer = MagicMock(name="tracer")
    meter = MagicMock(name="meter")
    tracer_provider = MagicMock(name="tracer_provider")
    meter_provider = MagicMock(name="meter_provider")
    tracer_provider.get_tracer.return_value = tracer
    meter_provider.get_meter.return_value = meter

    tracer_provider_factory = MagicMock(return_value=tracer_provider)
    meter_provider_factory = MagicMock(return_value=meter_provider)
    batch_processor_factory = MagicMock(side_effect=lambda exporter: ("batch", exporter))
    metric_reader_factory = MagicMock(side_effect=lambda exporter, **kwargs: (exporter, kwargs))
    console_span_exporter = MagicMock(return_value="console-span")
    console_metric_exporter = MagicMock(return_value="console-metric")
    resource = MagicMock()
    resource.create.return_value = "resource"
    trace_api = MagicMock()
    metrics_api = MagicMock()

    monkeypatch.setattr(telemetry, "_HAS_OTEL", True)
    monkeypatch.setattr(telemetry, "Resource", resource, raising=False)
    monkeypatch.setattr(telemetry, "TracerProvider", tracer_provider_factory, raising=False)
    monkeypatch.setattr(telemetry, "MeterProvider", meter_provider_factory, raising=False)
    monkeypatch.setattr(telemetry, "BatchSpanProcessor", batch_processor_factory, raising=False)
    monkeypatch.setattr(telemetry, "PeriodicExportingMetricReader", metric_reader_factory, raising=False)
    monkeypatch.setattr(telemetry, "ConsoleSpanExporter", console_span_exporter, raising=False)
    monkeypatch.setattr(telemetry, "ConsoleMetricExporter", console_metric_exporter, raising=False)
    monkeypatch.setattr(telemetry, "trace", trace_api, raising=False)
    monkeypatch.setattr(telemetry, "metrics", metrics_api, raising=False)

    return {
        "module": telemetry,
        "tracer": tracer,
        "meter": meter,
        "tracer_provider": tracer_provider,
        "meter_provider": meter_provider,
        "tracer_provider_factory": tracer_provider_factory,
        "meter_provider_factory": meter_provider_factory,
        "batch_processor_factory": batch_processor_factory,
        "metric_reader_factory": metric_reader_factory,
        "console_span_exporter": console_span_exporter,
        "console_metric_exporter": console_metric_exporter,
        "resource": resource,
        "trace_api": trace_api,
        "metrics_api": metrics_api,
    }


def test_telemetry_enabled_setup_is_owned_idempotent_and_shutdown(monkeypatch: pytest.MonkeyPatch) -> None:
    fake = _install_fake_otel(monkeypatch)
    manager = fake["module"].TelemetryManager(
        service_name="audit-test",
        export_interval_ms=1234,
    )

    manager.setup()
    manager.setup()  # must not install a second provider/export thread

    fake["resource"].create.assert_called_once_with({"service.name": "audit-test"})
    fake["tracer_provider_factory"].assert_called_once_with(resource="resource")
    fake["meter_provider_factory"].assert_called_once()
    fake["trace_api"].set_tracer_provider.assert_called_once_with(fake["tracer_provider"])
    fake["metrics_api"].set_meter_provider.assert_called_once_with(fake["meter_provider"])
    assert fake["metric_reader_factory"].call_args.kwargs == {"export_interval_millis": 1234}
    assert manager.get_tracer() is fake["tracer"]
    assert manager.get_meter() is fake["meter"]

    manager.shutdown()
    manager.shutdown()  # idempotent: providers are shut down exactly once

    fake["tracer_provider"].shutdown.assert_called_once_with()
    fake["meter_provider"].shutdown.assert_called_once_with()
    assert manager.get_tracer() is None
    assert manager.get_meter() is None
    assert manager._initialized is False


def test_telemetry_endpoint_falls_back_when_otlp_extra_missing(monkeypatch: pytest.MonkeyPatch) -> None:
    fake = _install_fake_otel(monkeypatch)
    manager = fake["module"].TelemetryManager(otel_endpoint="http://collector.invalid:4317")

    # The test environment deliberately has no exporter package. A configured
    # endpoint must still produce a functioning console pipeline.
    manager.setup()

    fake["console_span_exporter"].assert_called_once_with()
    fake["console_metric_exporter"].assert_called_once_with()
    manager.shutdown()


def test_telemetry_shutdown_releases_state_even_when_exporters_fail(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    fake = _install_fake_otel(monkeypatch)
    manager = fake["module"].TelemetryManager()
    manager.setup()
    fake["tracer_provider"].shutdown.side_effect = RuntimeError("trace exporter stuck")
    fake["meter_provider"].shutdown.side_effect = RuntimeError("metric exporter stuck")

    manager.shutdown()

    assert manager._initialized is False
    assert manager._tracer_provider is None
    assert manager._meter_provider is None


def test_telemetry_partial_setup_is_reaped_and_fails_open(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    fake = _install_fake_otel(monkeypatch)
    fake["meter_provider_factory"].side_effect = RuntimeError("meter init failed")
    manager = fake["module"].TelemetryManager()

    manager.setup()

    # Tracing was already constructed when metrics failed. It must be shut down
    # rather than leaving BatchSpanProcessor's worker thread behind, while the
    # optional diagnostics failure must not escape into AgentLoop construction.
    fake["tracer_provider"].shutdown.assert_called_once_with()
    assert manager.available is False
    assert manager.get_tracer() is None
    assert manager._tracer_provider is None


def test_telemetry_reaps_provider_when_span_processor_construction_fails(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    fake = _install_fake_otel(monkeypatch)
    exporter = MagicMock(name="span_exporter")
    fake["console_span_exporter"].return_value = exporter
    fake["batch_processor_factory"].side_effect = RuntimeError("processor failed")
    manager = fake["module"].TelemetryManager()

    manager.setup()

    fake["tracer_provider"].shutdown.assert_called_once_with()
    exporter.shutdown.assert_called_once_with()
    assert manager.available is False


def test_telemetry_reaps_unowned_metric_reader_when_provider_fails(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    fake = _install_fake_otel(monkeypatch)
    reader = MagicMock(name="metric_reader")
    fake["metric_reader_factory"].side_effect = None
    fake["metric_reader_factory"].return_value = reader
    fake["meter_provider_factory"].side_effect = RuntimeError("meter failed")
    manager = fake["module"].TelemetryManager()

    manager.setup()

    reader.shutdown.assert_called_once_with()
    fake["tracer_provider"].shutdown.assert_called_once_with()
    assert manager.available is False


def test_span_helpers_record_success_error_and_usage(monkeypatch: pytest.MonkeyPatch) -> None:
    from echo_agent.observability import spans

    monkeypatch.setattr(spans, "_HAS_OTEL", True)
    status_code = MagicMock()
    status_code.OK = "ok"
    status_code.ERROR = "error"
    monkeypatch.setattr(spans, "StatusCode", status_code, raising=False)
    tracer = MagicMock()
    span = MagicMock()
    tracer.start_span.return_value = span

    assert spans.start_llm_span(tracer, "model-a", "provider-a", "chat") is span
    tracer.start_span.assert_called_once_with(
        "gen_ai.chat",
        attributes={
            "gen_ai.system": "provider-a",
            "gen_ai.request.model": "model-a",
            "gen_ai.operation.name": "chat",
        },
    )

    spans.record_llm_usage(
        span,
        {
            "input_tokens": 10,
            "output_tokens": 3,
            "cache_read_input_tokens": 7,
            "cache_creation_input_tokens": 2,
        },
        model="model-b",
    )
    expected_attributes = {
        "gen_ai.response.model": "model-b",
        "gen_ai.usage.input_tokens": 10,
        "gen_ai.usage.output_tokens": 3,
        "gen_ai.usage.cache_read_input_tokens": 7,
        "gen_ai.usage.cache_creation_input_tokens": 2,
    }
    assert {call.args for call in span.set_attribute.call_args_list} == set(expected_attributes.items())

    spans.end_llm_span(span)
    span.set_status.assert_called_with("ok")
    spans.end_tool_span(span, "tool failed")
    span.set_status.assert_called_with("error", "tool failed")
    assert span.end.call_count == 2


def test_span_helpers_cover_tool_agent_and_zero_cache_usage(monkeypatch: pytest.MonkeyPatch) -> None:
    from echo_agent.observability import spans

    monkeypatch.setattr(spans, "_HAS_OTEL", True)
    tracer = MagicMock()
    span = MagicMock()
    tracer.start_span.return_value = span

    assert spans.start_tool_span(tracer, "exec") is span
    tracer.start_span.assert_called_with("tool.exec", attributes={"tool.name": "exec"})
    assert spans.start_agent_span(tracer, 2, strategy="react") is span
    tracer.start_span.assert_called_with(
        "agent.iteration",
        attributes={"gen_ai.agent.iteration": 2, "gen_ai.agent.strategy": "react"},
    )
    assert spans.start_agent_span(tracer, 3) is span
    tracer.start_span.assert_called_with("agent.iteration", attributes={"gen_ai.agent.iteration": 3})

    span.reset_mock()
    spans.record_llm_usage(span, {})
    assert {call.args for call in span.set_attribute.call_args_list} == {
        ("gen_ai.usage.input_tokens", 0),
        ("gen_ai.usage.output_tokens", 0),
    }


@pytest.mark.asyncio
async def test_health_checker_records_recovery_and_check_failures() -> None:
    from echo_agent.observability.monitor import ComponentHealth, HealthChecker

    checker = HealthChecker(check_interval=60)
    recovered: list[str] = []

    async def unhealthy() -> ComponentHealth:
        return ComponentHealth.UNHEALTHY

    async def recover() -> None:
        recovered.append("yes")

    async def broken() -> ComponentHealth:
        raise RuntimeError("probe exploded")

    checker.register_check("recoverable", unhealthy, recover)
    checker.register_check("broken", broken)
    statuses = await checker.check_all()

    assert recovered == ["yes"]
    assert statuses["recoverable"].status is ComponentHealth.UNHEALTHY
    assert statuses["recoverable"].message == "recovery attempted"
    assert statuses["recoverable"].last_check
    assert statuses["broken"].status is ComponentHealth.UNHEALTHY
    assert statuses["broken"].message == "probe exploded"
    assert statuses["broken"].last_check


@pytest.mark.asyncio
async def test_health_checker_records_recovery_failure() -> None:
    from echo_agent.observability.monitor import ComponentHealth, HealthChecker

    checker = HealthChecker()

    async def unhealthy() -> ComponentHealth:
        return ComponentHealth.UNHEALTHY

    async def recover() -> None:
        raise OSError("restart denied")

    checker.register_check("worker", unhealthy, recover)
    status = (await checker.check_all())["worker"]
    assert status.message == "recovery failed: restart denied"


@pytest.mark.asyncio
async def test_health_checker_start_stop_are_idempotent() -> None:
    from echo_agent.observability.monitor import HealthChecker

    checker = HealthChecker(check_interval=60)
    await checker.start()
    first_task = checker._task
    await checker.start()
    assert checker._task is first_task

    await asyncio.sleep(0)
    await checker.stop()
    await checker.stop()
    assert checker._task is None
    assert checker._running is False
