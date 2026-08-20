# 可观测性

Echo Agent 提供日志、指标和追踪三大观测维度，帮助你了解 Agent 的运行状态和行为。

---

## 观测能力概览

| 维度 | 实现 | 输出目标 |
|------|------|---------|
| 日志 | Loguru | 文件 / stdout / Gateway API |
| 追踪 | OpenTelemetry Traces | OTLP Collector |
| 指标 | OpenTelemetry Metrics | OTLP Collector / Prometheus |
| 成本 | 内置分析引擎 | CLI / Dashboard / API |
| 健康检查 | Gateway API | HTTP 端点 |

---

## 日志系统

Echo Agent 使用 [Loguru](https://github.com/Delgan/loguru) 作为日志框架，提供结构化、可配置的日志输出。

### 日志级别配置

日志级别配置在 `observability.log_level`。配置中没有 `logging` 节，也没有 `format`、`rotation`、`retention`、`compression` 这些字段 —— 日志格式与轮转由代码内的 loguru 配置决定，不对外暴露。

```yaml
# ~/.echo-agent/config.yaml
observability:
  log_level: INFO          # TRACE / DEBUG / INFO / WARNING / ERROR / CRITICAL
  max_trace_files: 500     # 追踪文件数量上限
```

日志目录由 `storage.logs_dir` 指定，默认 `data/logs`。

### 日志文件位置

```
~/.echo-agent/data/logs/
├── echo-agent.log           # 当前日志
├── echo-agent.log.1.gz      # 归档日志
├── echo-agent.log.2.gz
└── ...
```

### 运行时调整日志级别

```bash
# 前台模式：命令行覆盖
echo-agent run --log-level DEBUG

# 环境变量覆盖
ECHO_AGENT_LOG_LEVEL=DEBUG echo-agent run
```

### 结构化日志字段

Loguru 输出的关键字段：

| 字段 | 说明 |
|------|------|
| `time` | 时间戳 |
| `level` | 日志级别 |
| `name` | 模块名 |
| `function` | 函数名 |
| `message` | 日志内容 |
| `extra.session_id` | 会话 ID |
| `extra.task_id` | 任务 ID |
| `extra.tool_name` | 工具名称 |

---

## Gateway 日志 API

Gateway 运行时通过 API 提供日志访问：

```bash
# 查看最近日志
echo-agent gateway logs

# 等价 API 调用
curl -H "X-Echo-Agent-Token: $TOKEN" \
  http://localhost:58123/api/logs?lines=100&level=WARNING
```

### API 参数

| 参数 | 说明 | 默认值 |
|------|------|--------|
| `lines` | 返回行数 | 50 |
| `level` | 最低级别过滤 | INFO |
| `since` | 起始时间 (ISO 8601) | — |
| `until` | 结束时间 (ISO 8601) | — |
| `session_id` | 按会话过滤 | — |

---

## OpenTelemetry 集成

Echo Agent 支持 OpenTelemetry 协议导出追踪和指标数据。

### 启用 OTLP 导出

OTel 相关字段是 `observability` 下的平铺字段，前缀为 `otel_`，没有 `otlp` 嵌套节：

```yaml
# ~/.echo-agent/config.yaml
observability:
  otel_enabled: true                    # 默认 true
  otel_endpoint: "http://localhost:4317" # 为空时不导出
  otel_service_name: echo-agent
  otel_export_interval_ms: 5000
  trace_enabled: true
```

`otel_enabled` 默认已开启，但 `otel_endpoint` 默认为空 —— 未填写端点时不会导出任何数据。配置中没有 `protocol` 与 `headers` 字段。

### 追踪 (Traces)

每次 Agent 执行生成完整的追踪链路：

```
Agent Run (root span)
├── Model Call
│   ├── Token Count
│   └── Response Parse
├── Tool Execution: web_search
│   ├── HTTP Request
│   └── Result Parse
├── Memory Retrieval
│   └── Vector Search
└── Response Generation
```

关键 span 属性：

| 属性 | 说明 |
|------|------|
| `agent.session_id` | 会话标识 |
| `agent.task_id` | 任务标识 |
| `model.name` | 使用的模型 |
| `model.tokens_in` | 输入 token 数 |
| `model.tokens_out` | 输出 token 数 |
| `tool.name` | 工具名称 |
| `tool.duration_ms` | 工具执行耗时 |

### 指标 (Metrics)

导出的核心指标：

| 指标名 | 类型 | 说明 |
|--------|------|------|
| `echo_agent.requests_total` | Counter | 请求总数 |
| `echo_agent.model_calls_total` | Counter | 模型调用次数 |
| `echo_agent.model_tokens_total` | Counter | Token 消耗总量 |
| `echo_agent.tool_calls_total` | Counter | 工具调用次数 |
| `echo_agent.tool_duration_ms` | Histogram | 工具执行耗时 |
| `echo_agent.model_latency_ms` | Histogram | 模型响应延迟 |
| `echo_agent.active_sessions` | Gauge | 当前活跃会话数 |
| `echo_agent.cost_usd` | Counter | 累计成本（美元） |

### 对接 Grafana + Tempo + Prometheus

```yaml
# docker-compose.yml（观测基础设施）
version: "3.8"
services:
  otel-collector:
    image: otel/opentelemetry-collector-contrib:latest
    ports:
      - "4317:4317"    # gRPC
      - "4318:4318"    # HTTP
    volumes:
      - ./otel-config.yaml:/etc/otel/config.yaml

  prometheus:
    image: prom/prometheus:latest
    ports:
      - "9090:9090"
    volumes:
      - ./prometheus.yml:/etc/prometheus/prometheus.yml

  grafana:
    image: grafana/grafana:latest
    ports:
      - "3000:3000"
```

---

## 成本追踪

Echo Agent 内置 Token 用量和成本分析：

```bash
# 查看成本摘要
echo-agent cost

# 按时间范围查询
echo-agent cost --since 2024-01-01 --until 2024-01-31

# 按模型分组
echo-agent cost --group-by model
```

### 成本数据结构

| 字段 | 说明 |
|------|------|
| 模型 | 使用的 LLM 模型名 |
| 输入 Token | prompt tokens 数量 |
| 输出 Token | completion tokens 数量 |
| 成本 | 按模型定价计算的费用 |
| 时间 | 发生时间 |
| 会话 | 关联的会话 ID |

### 成本告警

```yaml
cost:
  enabled: true
  daily_budget_usd: 10.0      # 0 表示不限制
  soft_threshold_ratio: 0.8   # 达到 80% 预算时软预警
```

`cost` 没有 `budget` 嵌套节，也没有月度上限字段 —— 预算是按日的。详见[成本控制](../guides/cost-control.md)。

---

## 健康检查

Gateway 暴露健康检查端点：

```bash
# 基础健康检查
curl http://localhost:58123/health
# {"status": "healthy", "version": "0.3.7", "uptime_seconds": 3600}

# 详细状态（需 admin token）
curl -H "X-Echo-Agent-Token: $ADMIN_TOKEN" http://localhost:58123/health/detail
```

!!! tip "监控集成"
    健康检查端点可对接 uptime 监控服务（如 UptimeRobot、Healthchecks.io）或 Kubernetes liveness/readiness probe。

---

## 告警建议

| 指标 | 告警条件 | 建议阈值 |
|------|---------|---------|
| 健康检查 | 连续失败 | 3 次 |
| 模型延迟 | P99 过高 | > 30s |
| 错误率 | 5xx 比例 | > 5% |
| 磁盘用量 | 数据目录 | > 90% |
| 日成本 | 超出预算 | 80% 预算线 |
| 活跃会话 | 异常增长 | 按基线判断 |
