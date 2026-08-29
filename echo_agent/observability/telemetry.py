"""OpenTelemetry integration — TracerProvider, MeterProvider, GenAI conventions."""

from __future__ import annotations

from typing import Any

from loguru import logger

try:
    from opentelemetry import trace, metrics
    from opentelemetry.sdk.trace import TracerProvider
    from opentelemetry.sdk.trace.export import BatchSpanProcessor, ConsoleSpanExporter
    from opentelemetry.sdk.metrics import MeterProvider
    from opentelemetry.sdk.metrics.export import (
        ConsoleMetricExporter,
        PeriodicExportingMetricReader,
    )
    from opentelemetry.sdk.resources import Resource
    _HAS_OTEL = True
except ImportError:
    _HAS_OTEL = False


class TelemetryManager:
    """Manages OpenTelemetry providers and exporters.

    Gracefully degrades when opentelemetry is not installed.
    """

    def __init__(
        self,
        service_name: str = "echo-agent",
        otel_endpoint: str = "",
        export_interval_ms: int = 5000,
    ):
        self._service_name = service_name
        self._endpoint = otel_endpoint
        self._export_interval = export_interval_ms
        self._tracer: Any = None
        self._meter: Any = None
        self._tracer_provider: Any = None
        self._meter_provider: Any = None
        self._initialized = False

    @property
    def available(self) -> bool:
        # Dependency presence alone is not enough: a malformed exporter or
        # provider can fail during setup. Callers must not try to emit through a
        # half-built pipeline merely because the optional package imports.
        return _HAS_OTEL and self._initialized

    def setup(self) -> None:
        try:
            self._setup_otel()
        except Exception as e:
            # Telemetry is diagnostic infrastructure; an exporter/provider
            # failure must be visible but must not prevent the Agent from
            # starting. shutdown() also handles partially-created providers.
            logger.warning("OpenTelemetry setup failed — telemetry disabled: {}", e)
            self.shutdown()

    def _setup_otel(self) -> None:
        if not _HAS_OTEL:
            logger.info("OpenTelemetry not installed — telemetry disabled")
            return
        if self._initialized:
            return

        from echo_agent import __version__

        resource = Resource.create({"service.name": self._service_name})

        # Tracer
        tracer_provider = TracerProvider(resource=resource)
        # Own the provider before constructing exporters/processors. setup()
        # routes partial failures through shutdown(), so delaying this assignment
        # would leak a provider created just before an exporter failure.
        self._tracer_provider = tracer_provider
        if self._endpoint:
            try:
                from opentelemetry.exporter.otlp.proto.grpc.trace_exporter import OTLPSpanExporter
                exporter = OTLPSpanExporter(endpoint=self._endpoint)
            except ImportError:
                exporter = ConsoleSpanExporter()
        else:
            exporter = ConsoleSpanExporter()
        try:
            span_processor = BatchSpanProcessor(exporter)
        except BaseException:
            self._shutdown_partial(exporter)
            raise
        try:
            tracer_provider.add_span_processor(span_processor)
        except BaseException:
            # BatchSpanProcessor starts a worker in its constructor. Until the
            # provider accepts it, only this setup frame owns that worker.
            self._shutdown_partial(span_processor)
            raise
        trace.set_tracer_provider(tracer_provider)
        # Ask the provider we just built directly. OpenTelemetry deliberately
        # refuses to replace an already-installed global provider, so resolving
        # through ``trace.get_tracer`` could silently bind a second AgentLoop to
        # somebody else's provider in an embedded process.
        self._tracer = tracer_provider.get_tracer("echo-agent", __version__)

        # Meter. A MeterProvider with no reader silently drops every metric, so
        # export_interval_ms had nowhere to land: attach a periodic reader and
        # let it own the interval. Mirrors the tracer's OTLP/console fallback.
        metric_exporter: Any = None
        if self._endpoint:
            try:
                from opentelemetry.exporter.otlp.proto.grpc.metric_exporter import (
                    OTLPMetricExporter,
                )
                metric_exporter = OTLPMetricExporter(endpoint=self._endpoint)
            except ImportError:
                metric_exporter = ConsoleMetricExporter()
        else:
            metric_exporter = ConsoleMetricExporter()
        try:
            metric_reader = PeriodicExportingMetricReader(
                metric_exporter,
                export_interval_millis=self._export_interval,
            )
        except BaseException:
            self._shutdown_partial(metric_exporter)
            raise
        try:
            meter_provider = MeterProvider(
                resource=resource, metric_readers=[metric_reader],
            )
        except BaseException:
            # A rejected reader is not owned by MeterProvider, but may already
            # have started its periodic export worker.
            self._shutdown_partial(metric_reader)
            raise
        self._meter_provider = meter_provider
        metrics.set_meter_provider(meter_provider)
        self._meter = meter_provider.get_meter("echo-agent", __version__)

        self._initialized = True
        logger.info("OpenTelemetry initialized: service={}, endpoint={}", self._service_name, self._endpoint or "console")

    def get_tracer(self) -> Any:
        return self._tracer

    def get_meter(self) -> Any:
        return self._meter

    @staticmethod
    def _shutdown_partial(component: Any) -> None:
        """Best-effort close for a resource not yet adopted by a provider."""
        close = getattr(component, "shutdown", None)
        if not callable(close):
            return
        try:
            close()
        except Exception as e:
            logger.warning("OTel partial-resource shutdown error: {}", e)

    def shutdown(self) -> None:
        if not _HAS_OTEL:
            return
        if (
            not self._initialized
            and self._tracer_provider is None
            and self._meter_provider is None
        ):
            return
        try:
            if self._tracer_provider is not None and hasattr(self._tracer_provider, "shutdown"):
                self._tracer_provider.shutdown()
        except Exception as e:
            logger.warning("OTel shutdown error: {}", e)
        # The periodic metric reader owns a background export thread; without an
        # explicit shutdown it outlives the agent and the final batch is lost.
        try:
            if self._meter_provider is not None and hasattr(self._meter_provider, "shutdown"):
                self._meter_provider.shutdown()
        except Exception as e:
            logger.warning("OTel meter shutdown error: {}", e)
        finally:
            # Make shutdown idempotent and release references to exporters and
            # their worker threads. Global providers remain an OpenTelemetry
            # concern, but this manager must not keep a stopped pipeline alive.
            self._initialized = False
            self._tracer = None
            self._meter = None
            self._tracer_provider = None
            self._meter_provider = None
