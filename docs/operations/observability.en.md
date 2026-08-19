# Observability

Logging, monitoring, and telemetry for Echo Agent.

---

## Logging

Echo Agent uses [Loguru](https://github.com/Delgan/loguru) for structured logging.

### Log Levels

Configure via environment variable:

```bash
export ECHO_AGENT_OBSERVABILITY__LOG_LEVEL=DEBUG  # DEBUG, INFO, WARNING, ERROR
```

### Log Locations

- **Console**: stderr (foreground mode)
- **File**: `~/.echo-agent/logs/` (when running as service)
- **Gateway API**: `GET /api/v1/logs` (Dashboard Logs page)

## Gateway Health

```bash
curl http://127.0.0.1:58123/api/v1/health
```

Returns: status (healthy/degraded/unhealthy), active channels, WebSocket clients, provider status, rate limiter stats.

## OpenTelemetry

Enable OTLP export:

```yaml
observability:
  otelEnabled: true
  otelEndpoint: "http://localhost:4317"
```

Requires the `otel` extra:

```bash
pip install "echo-agent[otel]"
```

Exports traces and metrics via OTLP gRPC.

## Cost Analytics

```bash
echo-agent cost --days 7
echo-agent cost --days 30 --json
```

Dashboard Analytics page provides per-model token usage and cost breakdown.

## Sensitive Data

!!! warning
    Logs may contain conversation metadata. Review `observability` config for redaction settings before shipping logs to external systems.
