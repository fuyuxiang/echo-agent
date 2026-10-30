# Observability

**Scope: standalone Python Runtime.** For Desktop operations, see the [Desktop guide](../desktop/index.md).

Echo Agent offers stderr logs, an in-memory Gateway log query, execution-trace files, OpenTelemetry trace providers, and cost summaries. These use different storage and query paths.

## Logs

`observability.log_level` defaults to `INFO`. `configure_logging()` in `echo_agent/app.py` sends Loguru output to stderr and installs an in-memory ring buffer for the Dashboard. It does not automatically create `echo-agent.log` or rotated archive files. `storage.logs_dir` (default `data/logs`) holds execution traces and tool/memory audit files, rather than ordinary Loguru log files.

```bash
ECHO_AGENT_OBSERVABILITY__LOG_LEVEL=DEBUG echo-agent run
echo-agent gateway logs --follow
```

There is no `echo-agent run --log-level` option or top-level `ECHO_AGENT_LOG_LEVEL` configuration override.

Gateway `GET /api/v1/logs` reads the in-process buffer. It accepts `level` (exact level), `q` (message substring), `limit` (default 200), and `offset` (default 0), newest first. It requires an API token when tokens are configured:

```bash
curl -H "X-Echo-Agent-Token: $TOKEN" \
  "http://127.0.0.1:58123/api/v1/logs?limit=100&level=WARNING"
```

`observability.trace_enabled` controls internal execution-trace files. `observability.max_trace_files` defaults to 500 retained files. These files are separate from OpenTelemetry export.

## OpenTelemetry

With `echo-agent[otel]` installed, `observability.otel_enabled` (default `true`) initializes trace and meter providers. Set `otel_endpoint` to use OTLP gRPC. The default endpoint is empty, in which case `echo_agent/observability/telemetry.py` uses console exporters. Missing optional dependencies or initialization failures disable telemetry and log a diagnostic message.

```yaml
observability:
  otel_enabled: true
  otel_endpoint: http://127.0.0.1:4317
  otel_service_name: echo-agent
  otel_export_interval_ms: 5000
```

Current code creates spans for model calls, tool calls, and agent iterations with `gen_ai.*` and `tool.name` attributes. The telemetry module creates a `MeterProvider` and periodic reader, but it does not create instruments named `echo_agent.requests_total` or `echo_agent.cost_usd`. Do not configure alerts or Prometheus scrape jobs for those nonexistent instruments. The app does not start a Prometheus HTTP port by default.

## Cost and health

```bash
echo-agent cost --days 7
echo-agent cost --days 30 --json
curl http://127.0.0.1:58123/api/v1/health
```

The built-in cost tracker powers the Dashboard and `/api/v1/analytics/tokens`, `/api/v1/analytics/skills`, and `/api/v1/analytics/channels`. Health returns HTTP 200 or 503 according to status; there is no `/health/detail` route. See the [Gateway API](../reference/gateway-api.en.md) and [cost control](../guides/cost-control.en.md).
