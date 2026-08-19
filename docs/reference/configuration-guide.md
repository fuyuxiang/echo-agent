# 配置指南

Echo Agent 使用 YAML 格式的分层配置系统，支持多来源合并和环境变量覆盖。

## 配置加载顺序

配置按以下顺序加载，后加载的覆盖先加载的：

```
1. Package 内置默认值（最低优先级）
2. 用户 YAML 文件（-c 指定 或 ~/.echo-agent/config.yaml）
3. ECHO_AGENT_ 环境变量
4. CLI 运行时覆盖（--option 参数）
5. Profile 默认值
6. Pydantic 校验与类型转换（最高优先级）
```

!!! tip "查看最终配置"
    使用 `echo-agent config dump --show-source` 可查看每个字段的最终值及其来源。

---

## 配置文件位置

| 路径 | 作用域 | 说明 |
|------|--------|------|
| `~/.echo-agent/config.yaml` | 全局 | 用户级默认配置 |
| `.echo-agent/config.yaml` | 工作区 | 项目级配置（覆盖全局） |
| `-c <path>` | 手动指定 | CLI 参数指定的配置文件 |

---

## 顶层配置字段

完整的顶层字段列表：

| 字段 | 类型 | 说明 |
|------|------|------|
| `agent` | object | Agent 核心行为配置 |
| `security` | object | 安全策略与 Profile |
| `channels` | object | 通道集成配置（Slack/Telegram/Discord 等） |
| `models` | object | 模型提供商与参数 |
| `tools` | object | 工具系统配置与 Profile |
| `execution` | object | 执行环境参数 |
| `permissions` | object | 权限策略 |
| `credentials` | object | 凭据存储 |
| `session` | object | 会话管理 |
| `memory` | object | 长期记忆系统 |
| `knowledge` | object | 知识库配置 |
| `multi_agent` | object | 多 Agent 协作 |
| `scheduler` | object | 定时任务调度 |
| `checkpoint` | object | 检查点与快照 |
| `validation` | object | 输入输出校验规则 |
| `media_understanding` | object | 多媒体理解能力 |
| `runtime` | object | 运行时参数 |
| `storage` | object | 存储后端配置 |
| `spill` | object | 大文件溢出存储 |
| `observability` | object | 可观测性（日志/追踪/指标） |
| `skills` | object | 技能系统 |
| `compression` | object | 上下文压缩策略 |
| `gateway` | object | Gateway 服务配置 |
| `planning` | object | 规划与任务分解 |
| `a2a` | object | Agent-to-Agent 协议 |
| `evaluation` | object | 评估框架 |
| `bus` | object | 事件总线 |
| `rate_limit` | object | 频率限制 |
| `circuit_breaker` | object | 熔断器 |
| `plugins` | object | 插件系统 |
| `ui` | object | UI 配置 |
| `evolution` | object | 技能进化系统 |
| `cost` | object | 费用管理 |
| `workspace` | object | 工作区设置 |

---

## 核心配置详解

### agent

```yaml
agent:
  name: "my-echo-agent"          # Agent 实例名称
  persona: "你是一个有帮助的助手"  # 系统人设提示词
  language: "zh-CN"               # 首选回复语言
  max_iterations: 20              # 单次任务最大迭代数
  idle_timeout: 3600              # 空闲超时（秒）
```

### security

```yaml
security:
  profile: standard              # 安全级别: minimal / standard / extended
  sandbox: true                  # 启用沙箱隔离
  allowed_paths:                 # 允许访问的文件路径
    - /home/user/projects
    - /tmp/echo-agent
  blocked_commands:              # 禁止执行的命令
    - rm -rf /
    - format
```

Profile 详见 [安全配置矩阵](security-profile-matrix.md)。

### models

```yaml
models:
  primary:
    provider: anthropic          # 提供商: anthropic / openai / google / local
    model: claude-sonnet-4-20250514  # 模型标识
    api_key: ${ANTHROPIC_API_KEY}  # API Key（支持环境变量引用）
    base_url: null               # 自定义端点（用于代理/本地模型）
    max_tokens: 4096             # 最大输出 token
    temperature: 0.7             # 采样温度
  fallback:                      # 备选模型（主模型失败时使用）
    provider: openai
    model: gpt-4o
    api_key: ${OPENAI_API_KEY}
```

### tools

```yaml
tools:
  profile: messaging             # 工具级别: minimal / messaging / coding / full
  approval_mode: ask             # 审批模式: auto / ask / deny
  timeout: 300                   # 工具执行超时（秒）
  overrides:                     # 单工具级别覆盖
    shell:
      approval_mode: ask
      timeout: 60
    filesystem:
      approval_mode: auto
      allowed_paths:
        - ./workspace
```

### channels

```yaml
channels:
  slack:
    enabled: true
    bot_token: ${SLACK_BOT_TOKEN}
    app_token: ${SLACK_APP_TOKEN}
    allowed_channels:
      - general
      - dev-team
  telegram:
    enabled: true
    bot_token: ${TELEGRAM_BOT_TOKEN}
    allowed_users:
      - 123456789
  discord:
    enabled: false
    bot_token: ${DISCORD_BOT_TOKEN}
```

### gateway

```yaml
gateway:
  host: 127.0.0.1
  port: 8080
  auth:
    mode: pairing                # open / allowlist / pairing
    api_tokens:
      - "token-abc-123"
    admin_tokens:
      - "admin-xyz-789"
    allowed_origins:
      - "http://localhost:3000"
    allowed_hosts:
      - "localhost"
    allowed_users: []
    admin_users: []
    token_header: "X-Echo-Token"
    pairing_ttl_seconds: 300
  cors:
    enabled: true
  tls:
    enabled: false
    cert_file: null
    key_file: null
```

### memory

```yaml
memory:
  backend: local                 # 存储后端: local / postgres
  auto_save: true                # 自动保存对话记忆
  consolidation_interval: 3600   # 记忆整合间隔（秒）
  max_entries: 10000             # 最大记忆条目数
  embedding:
    enabled: true
    model: text-embedding-3-small
```

### scheduler

```yaml
scheduler:
  enabled: true
  max_concurrent: 5              # 最大并发任务数
  timezone: "Asia/Shanghai"      # 时区
  miss_policy: skip              # 错过执行策略: skip / run_once / run_all
```

### checkpoint

```yaml
checkpoint:
  enabled: true
  interval: 3600                 # 自动检查点间隔（秒）
  max_count: 50                  # 最大保留数量
  auto_prune: true               # 自动清理过期检查点
  retention_days: 30             # 保留天数
```

### observability

```yaml
observability:
  log_level: INFO                # DEBUG / INFO / WARNING / ERROR
  log_file: null                 # 日志文件路径
  otel_enabled: false            # OpenTelemetry 启用
  otel_endpoint: null            # OTel Collector 端点
  metrics_port: 9090             # Prometheus 指标端口
  trace_sampling_rate: 0.1       # 追踪采样率
```

### cost

```yaml
cost:
  tracking_enabled: true
  daily_limit: 10.0              # 每日上限（USD）
  monthly_limit: 200.0           # 每月上限（USD）
  alert_threshold: 0.8           # 预警阈值（占比）
  alert_channel: null            # 预警通知通道
```

### rate_limit

```yaml
rate_limit:
  enabled: true
  requests_per_minute: 60
  tokens_per_minute: 100000
  burst_multiplier: 1.5          # 突发倍率
```

### circuit_breaker

```yaml
circuit_breaker:
  enabled: true
  failure_threshold: 5           # 连续失败次数阈值
  recovery_timeout: 60           # 恢复等待时间（秒）
  half_open_requests: 3          # 半开状态试探请求数
```

---

## Profile 系统

Profile 是预定义的配置模板，简化常见场景配置。

### 安全 Profile

| Profile | 说明 |
|---------|------|
| `minimal` | 最低安全限制，适合本地开发 |
| `standard` | 平衡安全与功能（默认） |
| `extended` | 最严格，适合生产环境 |

### 工具 Profile

| Profile | 包含工具范围 |
|---------|-------------|
| `minimal` | 仅只读工具 |
| `messaging` | + 消息与媒体工具 |
| `coding` | + 文件写入与代码执行 |
| `full` | 所有工具（含高风险） |

---

## 环境变量引用

配置文件中支持 `${VAR_NAME}` 语法引用环境变量：

```yaml
models:
  primary:
    api_key: ${ANTHROPIC_API_KEY}    # 从环境变量读取
    base_url: ${API_PROXY:-null}     # 支持默认值语法
```

---

## 配置校验

```bash
# 校验配置文件
echo-agent config validate

# 校验并显示错误详情
echo-agent config validate -c ./config.yaml --verbose
```

常见校验错误：

| 错误 | 原因 | 解决方案 |
|------|------|----------|
| `Unknown field` | YAML 中存在未知字段 | 检查拼写或移除该字段 |
| `Type error` | 类型不匹配 | 按文档修正值类型 |
| `Missing required` | 缺少必填字段 | 补充必填值 |
| `Invalid profile` | Profile 名称无效 | 使用有效的 Profile 名 |

!!! question "需维护者确认"
    配置文件中的 `${VAR:-default}` 默认值语法是否已完整实现？当前文档基于预期行为描述。
