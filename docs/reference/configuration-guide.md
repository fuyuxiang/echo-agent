# 配置指南

本页讲配置的**机制**：文件在哪、如何加载、如何覆盖、如何校验。逐个字段的含义、类型与默认值请查[配置参考](configuration.md) —— 那一页由 `echo_agent.config.docgen` 从 schema 自动生成，永远与代码同步。

!!! note "两页的分工"
    本页解释规则，不复制字段清单。手工维护的字段表会随代码演进而失准，因此这里只列出各配置节的用途与入口，具体字段一律以自动生成页和 `echo_agent/config/schema.py` 为准。

## 配置加载顺序

`load_config()` 依次合并四个来源，后者覆盖前者：

| 顺序 | 来源 | 说明 |
|------|------|------|
| 1 | `echo_agent/config/default.yaml` | 包内默认配置，随版本发布 |
| 2 | 用户配置文件 | 见下节的查找规则 |
| 3 | `ECHO_AGENT_` 环境变量 | 见[环境变量参考](environment-variables.md) |
| 4 | 调用方显式 overrides | 供程序化调用使用 |

合并是**深合并**：只覆盖同名叶子字段，同级其他字段保留原值。因此用户配置只需写要改的部分，不必复制整份默认配置。

## 配置文件位置

未通过 `--config` 指定路径时，按以下文件名在搜索目录中依次查找，取第一个存在的：

1. `echo-agent.yaml`
2. `echo-agent.yml`
3. `config.yaml`
4. `config.yml`

## 顶层配置节

配置树共 33 个配置节加 1 个标量字段 `workspace`。按用途分组如下：

| 分组 | 配置节 |
|------|--------|
| 模型与推理 | `models`、`agent`、`planning`、`compression` |
| 工具与执行 | `tools`、`execution`、`permissions`、`security` |
| 通道与网关 | `channels`、`gateway`、`bus`、`a2a` |
| 记忆与知识 | `memory`、`knowledge`、`session`、`spill` |
| 任务与技能 | `scheduler`、`skills`、`evolution`、`multi_agent` |
| 运行与运维 | `runtime`、`storage`、`checkpoint`、`observability` |
| 成本与稳定性 | `cost`、`rate_limit`、`circuit_breaker` |
| 其他 | `credentials`、`plugins`、`ui`、`validation`、`evaluation`、`media_understanding`、`workspace` |

### 常用配置节速览

以下只列出各节最常调整的入口字段及其真实默认值。

**models** — 模型与供应商。`default_model`、`fallback_model` 指定模型；`providers` 是供应商列表；`routes` 按任务类型分流；`model_windows` 覆盖上下文窗口。

```yaml
models:
  default_model: claude-sonnet-4-5
  providers:
    - name: anthropic          # 供应商标识，不是 type
      # api_key 可省略：留空时从 ANTHROPIC_API_KEY 环境变量自动发现
      models: [claude-sonnet-4-5]
```

!!! warning "不存在 models.primary"
    供应商由 `providers` 列表描述，其判别字段是 `name` 而非 `type`；模型由 `default_model` / `fallback_model` 指定。写成 `models.primary.provider` 这类结构不会报错，但会被 pydantic 当作未知键**静默忽略**，最终得到一份空模型配置。详见[模型配置指南](../guides/models/index.md)。

**tools** — 工具准入。`profile` 选档位（默认 `full`）；`allow` / `also_allow` / `deny` 精细控制；`restrict_to_workspace` 限制文件操作范围。各工具的独立开关是嵌套节，如 `tools.exec`、`tools.browser`、`tools.web`。

**security** — 只有一个字段 `profile`，取值 `personal_cli`（默认）/ `daemon` / `public_gateway`。

**permissions** — 审批与提权，含 `approval`、`elevated`、`admin_users` 三部分。审批模式在 `permissions.approval.mode`，取值 `manual` / `smart`（默认）/ `off`。

```yaml
tools:
  profile: coding
security:
  profile: daemon
permissions:
  approval:
    mode: smart
```

准入与审批的完整判定顺序见[安全档位矩阵](security-profile-matrix.md)。

**gateway** — HTTP/WebSocket 网关。`enabled` 默认 `false`；`host` 默认 `127.0.0.1`；`port` 默认 `58123`；`api_prefix` 默认 `/api/v1`；`ws_path` 默认 `/ws`。鉴权在 `gateway.auth`，会话策略在 `gateway.session_policy`。

```yaml
gateway:
  enabled: true
  host: 127.0.0.1      # 改为 0.0.0.0 即对外暴露，须同时启用鉴权
  port: 58123
```

**execution** — 执行后端。`default_executor` 默认 `sandbox`；`network_policy` 默认 `deny`，改为 `allow` 或 `restricted` 才允许出站访问。

**observability** — 日志与追踪。`log_level` 默认 `INFO`；`trace_enabled`、`otel_enabled` 默认开启；`otel_endpoint` 为空时不导出。

**cost** — 成本控制。`enabled` 默认 `false`；`daily_budget_usd` 默认 `0.0`（不限制）；`soft_threshold_ratio` 默认 `0.8`。

**rate_limit** — 限流。`session_rpm` 默认 `20`，`session_burst` 默认 `5`。

**circuit_breaker** — 熔断。`failure_threshold` 默认 `5`，`recovery_seconds` 默认 `60.0`，`half_open_max` 默认 `2`。

**checkpoint** — 工作区快照。`enabled` 默认 `true`；`max_snapshots_per_workspace` 默认 `20`。快照**不含**数据库、会话、记忆与日志目录：对运行中的 SQLite 做文件级快照会得到撕裂的读取结果。

**memory** — 记忆系统，字段较多（40 余项），涵盖分层、嵌入、重排与矛盾检测。默认启用且带本地嵌入模型，一般无需调整，详见[记忆系统](../concepts/memory-system.md)。

### 字段命名

配置同时接受 snake_case 与 camelCase 两种写法，`allow_from` 与 `allowFrom` 等价。本文档统一使用 snake_case；`echo-agent config dump` 的输出为 camelCase，两者可以混用而不影响解析。

## 环境变量引用

!!! warning "配置文件不做变量替换"
    配置值中的 `${VAR}` **不会**被展开，而是原样保留为字面字符串。写成 `api_key: "${ANTHROPIC_API_KEY}"` 会把这串字符本身当作 API Key。

正确的做法有两种。其一，完全不在配置里写凭据 —— 供应商 API Key 会从约定的环境变量自动发现：

```bash
export ANTHROPIC_API_KEY=sk-ant-...
```

其二，用环境变量覆盖对应配置项（仅适用于标量字段，列表类型不支持）：

```bash
export ECHO_AGENT_MODELS__DEFAULT_MODEL=claude-sonnet-4-5
```

自动发现的变量名对照表与覆盖规则见[环境变量参考](environment-variables.md)。

## 配置校验

配置由 pydantic 校验，行为分两类，理解这一点对排查很关键：

- **类型或取值非法** — 启动即报错并中止。例如把 `security.profile` 写成 `standard`（合法值只有三个），或把 `gateway.port` 写成非数字。
- **未知字段** — 静默忽略，不报错、不警告。因此拼错字段名或用了不存在的结构时，表现为"配置没生效"而非报错。

用以下命令在启动前检查配置：

```bash
echo-agent config validate
```

`config` 子命令共四个动作：

| 命令 | 作用 |
|------|------|
| `echo-agent config validate` | 校验配置合法性 |
| `echo-agent config dump` | 输出合并后实际生效的配置（凭据自动脱敏），可加 `--format json` |
| `echo-agent config explain <key>` | 解释某个配置项，接受点分路径 |
| `echo-agent config gen-docs` | 重新生成配置参考页 |

排查"某项配置似乎没生效"时，优先怀疑第二类情况：用 `echo-agent config dump` 查看合并后的实际值，或用 `echo-agent config explain gateway.port` 确认字段路径拼写正确。

## 相关页面

- [配置参考](configuration.md) — 由 schema 自动生成的逐项说明
- [环境变量参考](environment-variables.md) — 覆盖规则与凭据变量
- [安全档位矩阵](security-profile-matrix.md) — 工具准入与审批判定
