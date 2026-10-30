# 可观测性

**适用范围：独立 Python Runtime。** 桌面端操作见 [Desktop 使用指南](../desktop/index.md)。

Echo Agent 提供 stderr 日志、Gateway 内存日志查询、执行轨迹文件、OpenTelemetry 追踪提供器与成本统计。各功能使用不同的存储和查询入口。

## 日志

`observability.log_level` 默认 `INFO`。`echo_agent/app.py` 的 `configure_logging()` 把 Loguru 日志输出到 stderr，并安装供 Dashboard 查询的内存环形缓冲区；它没有自动创建 `echo-agent.log` 及归档文件。`storage.logs_dir`（默认 `data/logs`）用于执行轨迹、工具和记忆审计等文件，而非普通 Loguru 日志文件。

```bash
ECHO_AGENT_OBSERVABILITY__LOG_LEVEL=DEBUG echo-agent run
echo-agent gateway logs --follow
```

没有 `echo-agent run --log-level` 参数，也没有顶层 `ECHO_AGENT_LOG_LEVEL` 配置覆盖变量。

Gateway 的 `GET /api/v1/logs` 从进程内缓冲区读取日志，接受 `level`（精确级别）、`q`（消息子串）、`limit`（默认 200）和 `offset`（默认 0）；最新记录排在前面。此接口要求 API 令牌（如果配置了令牌）：

```bash
curl -H "X-Echo-Agent-Token: $TOKEN" \
  "http://127.0.0.1:58123/api/v1/logs?limit=100&level=WARNING"
```

`observability.trace_enabled` 控制内部执行轨迹文件，`observability.max_trace_files` 默认保留最多 500 个文件。这些文件与 OpenTelemetry 导出是不同机制。

## OpenTelemetry

安装 `echo-agent[otel]` 后，`observability.otel_enabled`（默认 `true`）会初始化追踪和指标提供器。配置 `otel_endpoint` 时使用 OTLP gRPC 导出；默认端点为空，此时 `echo_agent/observability/telemetry.py` 使用控制台导出器。缺少可选依赖或初始化失败时，遥测会停用并记录日志。

```yaml
observability:
  otel_enabled: true
  otel_endpoint: http://127.0.0.1:4317
  otel_service_name: echo-agent
  otel_export_interval_ms: 5000
```

当前代码在模型调用、工具调用和 Agent 迭代处创建 span，使用 `gen_ai.*` 与 `tool.name` 属性。遥测模块创建 `MeterProvider` 和周期性读取器，但没有创建文档曾列出的 `echo_agent.requests_total`、`echo_agent.cost_usd` 等指标工具。不要据此配置这些指标的告警或 Prometheus 抓取任务；代码也没有默认启动 Prometheus HTTP 端口。

## 成本与健康状态

```bash
echo-agent cost --days 7
echo-agent cost --days 30 --json
curl http://127.0.0.1:58123/api/v1/health
```

成本由内置跟踪器汇总，并可通过 Dashboard 及 `/api/v1/analytics/tokens`、`/api/v1/analytics/skills`、`/api/v1/analytics/channels` 查询。健康接口根据状态返回 HTTP 200 或 503；没有 `/health/detail` 路由。详见[Gateway API](../reference/gateway-api.md)与[成本控制](../guides/cost-control.md)。
