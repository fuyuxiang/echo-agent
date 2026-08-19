# 环境变量参考

Echo Agent 使用 `ECHO_AGENT_` 前缀的环境变量覆盖配置文件中的设置。

## 命名规则

### 前缀

所有环境变量必须使用 `ECHO_AGENT_` 前缀。

### 嵌套分隔

使用双下划线 `__` 表示配置层级嵌套：

```bash
# 对应 YAML 配置:
# gateway:
#   auth:
#     mode: pairing
ECHO_AGENT_GATEWAY__AUTH__MODE=pairing
```

### 类型转换

| 目标类型 | 环境变量值示例 | 转换规则 |
|----------|---------------|----------|
| `str` | `hello` | 原样使用 |
| `int` | `8080` | `int()` 转换 |
| `float` | `0.95` | `float()` 转换 |
| `bool` | `true` / `1` / `yes` | 不区分大小写，视为 True |
| `list` | `a,b,c` | 逗号分隔 |
| `None` | `null` / `none` / 空串 | 视为 None |

---

## 配置覆盖优先级

环境变量在配置加载链中的位置：

```
Package 默认值（最低）
    ↓
用户 YAML（-c 指定或 ~/.echo-agent/config.yaml）
    ↓
ECHO_AGENT_ 环境变量       ← 此处
    ↓
CLI 运行时覆盖（--option）
    ↓
Profile 默认值
    ↓
Pydantic 校验（最高）
```

!!! tip "调试优先级"
    使用 `echo-agent config dump --show-source` 可查看每个字段的最终值来源。

---

## 核心环境变量

### 通用

| 变量 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `ECHO_AGENT_CONFIG` | str | `~/.echo-agent/config.yaml` | 配置文件路径 |
| `ECHO_AGENT_DATA_DIR` | str | `~/.echo-agent/` | 全局数据目录 |
| `ECHO_AGENT_WORKSPACE` | str | `.echo-agent/` | 工作区数据目录 |
| `ECHO_AGENT_LOG_LEVEL` | str | `INFO` | 日志级别（DEBUG/INFO/WARNING/ERROR） |
| `ECHO_AGENT_LOG_FILE` | str | — | 日志文件路径（不设置则输出到 stderr） |

### 模型配置

| 变量 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `ECHO_AGENT_MODELS__PRIMARY__PROVIDER` | str | — | 主模型提供商 |
| `ECHO_AGENT_MODELS__PRIMARY__MODEL` | str | — | 主模型名称 |
| `ECHO_AGENT_MODELS__PRIMARY__API_KEY` | str | — | 主模型 API Key |
| `ECHO_AGENT_MODELS__PRIMARY__BASE_URL` | str | — | 自定义 API 端点 |
| `ECHO_AGENT_MODELS__PRIMARY__MAX_TOKENS` | int | `4096` | 最大输出 token 数 |
| `ECHO_AGENT_MODELS__PRIMARY__TEMPERATURE` | float | `0.7` | 采样温度 |

### Gateway

| 变量 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `ECHO_AGENT_GATEWAY__HOST` | str | `127.0.0.1` | 监听地址 |
| `ECHO_AGENT_GATEWAY__PORT` | int | `8080` | 监听端口 |
| `ECHO_AGENT_GATEWAY__AUTH__MODE` | str | `pairing` | 认证模式（open/allowlist/pairing） |
| `ECHO_AGENT_GATEWAY__AUTH__API_TOKENS` | list | — | API token 列表（逗号分隔） |
| `ECHO_AGENT_GATEWAY__AUTH__ADMIN_TOKENS` | list | — | 管理 token 列表 |
| `ECHO_AGENT_GATEWAY__AUTH__ALLOWED_ORIGINS` | list | — | 允许的 Origin 列表 |
| `ECHO_AGENT_GATEWAY__AUTH__ALLOWED_HOSTS` | list | — | 允许的 Host 列表 |
| `ECHO_AGENT_GATEWAY__AUTH__TOKEN_HEADER` | str | `X-Echo-Token` | Token 头名称 |
| `ECHO_AGENT_GATEWAY__AUTH__PAIRING_TTL_SECONDS` | int | `300` | 配对码有效期（秒） |

### 安全

| 变量 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `ECHO_AGENT_SECURITY__PROFILE` | str | `standard` | 安全 Profile（minimal/standard/extended） |
| `ECHO_AGENT_SECURITY__SANDBOX` | bool | `true` | 是否启用沙箱 |
| `ECHO_AGENT_TOOLS__PROFILE` | str | `messaging` | 工具 Profile（minimal/messaging/coding/full） |

### 执行环境

| 变量 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `ECHO_AGENT_EXECUTION__TIMEOUT` | int | `300` | 工具执行超时（秒） |
| `ECHO_AGENT_EXECUTION__MAX_RETRIES` | int | `3` | 工具调用最大重试次数 |
| `ECHO_AGENT_EXECUTION__SHELL` | str | `/bin/bash` | Shell 执行环境路径 |

### 存储

| 变量 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `ECHO_AGENT_STORAGE__BACKEND` | str | `sqlite` | 存储后端（sqlite/postgres） |
| `ECHO_AGENT_STORAGE__SQLITE__PATH` | str | `data/sqlite/echo.db` | SQLite 数据库路径 |
| `ECHO_AGENT_STORAGE__POSTGRES__DSN` | str | — | PostgreSQL 连接字符串 |

### 可观测性

| 变量 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `ECHO_AGENT_OBSERVABILITY__OTEL_ENDPOINT` | str | — | OpenTelemetry Collector 端点 |
| `ECHO_AGENT_OBSERVABILITY__OTEL_ENABLED` | bool | `false` | 是否启用 OTel 导出 |
| `ECHO_AGENT_OBSERVABILITY__METRICS_PORT` | int | `9090` | Prometheus metrics 端口 |

### 会话与记忆

| 变量 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `ECHO_AGENT_SESSION__MAX_HISTORY` | int | `100` | 会话最大历史消息数 |
| `ECHO_AGENT_MEMORY__BACKEND` | str | `local` | 记忆存储后端 |
| `ECHO_AGENT_MEMORY__AUTO_SAVE` | bool | `true` | 是否自动保存记忆 |

### 费用控制

| 变量 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `ECHO_AGENT_COST__DAILY_LIMIT` | float | — | 每日费用上限（USD） |
| `ECHO_AGENT_COST__MONTHLY_LIMIT` | float | — | 每月费用上限（USD） |
| `ECHO_AGENT_COST__ALERT_THRESHOLD` | float | `0.8` | 费用预警阈值（占比） |

### 频率限制

| 变量 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `ECHO_AGENT_RATE_LIMIT__REQUESTS_PER_MINUTE` | int | `60` | 每分钟请求上限 |
| `ECHO_AGENT_RATE_LIMIT__TOKENS_PER_MINUTE` | int | `100000` | 每分钟 token 上限 |

---

## 凭据环境变量

模型 API Key 等敏感信息推荐通过环境变量传递，避免写入配置文件：

```bash
# 模型 API Keys
export ECHO_AGENT_CREDENTIALS__ANTHROPIC_API_KEY=sk-ant-...
export ECHO_AGENT_CREDENTIALS__OPENAI_API_KEY=sk-...
export ECHO_AGENT_CREDENTIALS__GOOGLE_API_KEY=AIza...

# 通道凭据
export ECHO_AGENT_CREDENTIALS__SLACK_BOT_TOKEN=xoxb-...
export ECHO_AGENT_CREDENTIALS__TELEGRAM_BOT_TOKEN=123456:ABC...
export ECHO_AGENT_CREDENTIALS__DISCORD_BOT_TOKEN=MTI...
```

!!! danger "安全提醒"
    切勿将 API Key 写入版本控制。使用 `.env` 文件时确保已加入 `.gitignore`。

---

## 调试相关变量

| 变量 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `ECHO_AGENT_DEBUG` | bool | `false` | 启用调试模式（详细日志 + 错误堆栈） |
| `ECHO_AGENT_TRACE` | bool | `false` | 启用请求追踪（极详细） |
| `ECHO_AGENT_DRY_RUN` | bool | `false` | 干运行模式（不执行实际工具调用） |
| `ECHO_AGENT_PROFILE_PERF` | bool | `false` | 启用性能分析 |

---

## 使用示例

### Docker Compose 中使用

```yaml
services:
  echo-agent:
    image: echo-agent:latest
    environment:
      - ECHO_AGENT_GATEWAY__HOST=0.0.0.0
      - ECHO_AGENT_GATEWAY__PORT=8080
      - ECHO_AGENT_GATEWAY__AUTH__MODE=allowlist
      - ECHO_AGENT_MODELS__PRIMARY__PROVIDER=anthropic
      - ECHO_AGENT_MODELS__PRIMARY__MODEL=claude-sonnet-4-20250514
      - ECHO_AGENT_CREDENTIALS__ANTHROPIC_API_KEY=${ANTHROPIC_API_KEY}
      - ECHO_AGENT_SECURITY__PROFILE=standard
      - ECHO_AGENT_LOG_LEVEL=INFO
```

### systemd 服务文件中使用

```ini
[Service]
Environment=ECHO_AGENT_GATEWAY__HOST=127.0.0.1
Environment=ECHO_AGENT_GATEWAY__PORT=8080
Environment=ECHO_AGENT_SECURITY__PROFILE=extended
EnvironmentFile=/etc/echo-agent/env
```

### Shell 临时覆盖

```bash
# 临时使用调试模式运行
ECHO_AGENT_DEBUG=true ECHO_AGENT_LOG_LEVEL=DEBUG echo-agent run

# 临时切换模型
ECHO_AGENT_MODELS__PRIMARY__MODEL=claude-sonnet-4-20250514 echo-agent run
```

!!! question "需维护者确认"
    是否支持 `.env` 文件自动加载？当前行为需要确认：仅从工作目录加载还是同时检查 `~/.echo-agent/.env`。
