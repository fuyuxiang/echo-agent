# 路由与 Fallback 策略

本文档说明 Echo-Agent 如何根据任务类型将请求路由到特定模型，以及当模型不可用时如何执行降级回退。

## 路由匹配逻辑

`ModelRouter` 按以下优先级解析目标模型：

1. **preferred_model** — 调用方显式指定的模型，最高优先级
2. **task_type 匹配** — 根据请求中的 `task_type` 在路由表中查找匹配的 route
3. **default_model** — 以上均未命中时使用全局默认模型

匹配成功后，`RouteDecision` 将包含：

| 字段 | 说明 |
|------|------|
| `provider_name` | 提供商标识 |
| `model` | 选中的模型 ID |
| `fallback_chain` | 降级链（有序列表） |
| `reason` | 路由命中原因 |
| `context_window` | 上下文窗口大小 |
| `max_tokens` | 最大输出 token 数 |
| `temperature` | 采样温度 |

### task_type 的取值

`task_type` 由框架根据用户输入自动推断，共四种取值：

| 取值 | 触发条件 |
|------|----------|
| `code` | 文本含代码相关词，如「代码」「报错」「函数」`bug`、`class `、`def `、`typescript`、`python` |
| `research` | 文本含检索意图词，如「搜索」「查找」「查一下」`search`、`find`、`look up` |
| `planning` | 文本含规划意图词，如「计划」「规划」「安排」`plan`、`schedule` |
| `chat` | 未命中上述任一标记时的兜底值 |

`models.routes[].task_types` 中填写的字符串与推断结果做不区分大小写的匹配。此外，当路由未配置 `task_types` 时，`task_type` 与该路由的 `provider` 同名、或作为子串出现在 `model` 中，也视为命中。

## 健康状态机

每个 Provider 维护独立的 `ProviderHealth` 实例，状态转换如下：

```
┌─────────┐  连续失败达阈值   ┌──────────┐  120s 冷却期满   ┌───────────┐
│ HEALTHY │ ───────────────→ │ COOLDOWN │ ──────────────→ │ HALF_OPEN │
└─────────┘                  └──────────┘                  └───────────┘
     ↑                            ↑                          │       │
     │                            │    探测失败（2次机会内）   │       │
     │                            └──────────────────────────┘       │
     │         探测成功                                              │
     └──────────────────────────────────────────────────────────────┘
```

### 状态说明

| 状态 | 含义 |
|------|------|
| `HEALTHY` | 正常可用 |
| `DEGRADED` | 性能下降但仍可接受请求 |
| `COOLDOWN` | 冷却中，拒绝所有请求（默认 120 秒） |
| `HALF_OPEN` | 允许最多 2 次探测请求验证恢复 |
| `DISABLED` | 手动禁用，不参与路由 |

### ProviderHealth 跟踪字段

- `failure_count` — 连续失败计数
- `last_error` — 最近一次错误信息
- `cooldown_until` — 冷却期结束时间戳
- `half_open_allowance` — 半开状态允许的探测次数（最大 2）

!!! note "半开探测机制"
    进入 `HALF_OPEN` 后，最多放行 2 个请求作为探测：
    - 探测成功 → 状态恢复为 `HEALTHY`
    - 探测失败 → 立即回到 `COOLDOWN`，重新计时

## Fallback 链解析

当主模型不可用（非 `HEALTHY` 或 `HALF_OPEN`）时，`route_candidates()` 构建完整降级链：

```
主模型 (primary)
  ↓ 不可用
route 级 fallback_models（按顺序尝试）
  ↓ 全部不可用
全局 fallback_model
```

### 解析步骤

1. 尝试 `RouteDecision.model`（主模型）
2. 依次尝试该 route 配置的 `fallback_models` 列表
3. 最终回退到全局 `fallback_model`
4. 每一步都检查目标 Provider 的健康状态，跳过非健康节点

!!! warning "全部降级失败"
    如果 fallback 链中所有模型的 Provider 均处于非可用状态，请求将返回错误。
    建议至少保留一个高可用的全局 fallback_model。

## 上下文窗口解析

`context_window` 通过 `model_windows` 配置映射获取：

```yaml
models:
  model_windows:
    "gpt-4o": 128000
    "claude-sonnet-4-20250514": 200000
    "gpt-4o-mini": 128000
    "deepseek-chat": 64000
```

路由决策时，`context_window` 从匹配到的模型名在 `model_windows` 中查找。若未配置对应条目，将使用系统内置的默认值。

## 配置示例

```yaml
models:
  default_model: "gpt-4o"
  fallback_model: "gpt-4o-mini"

  routes:
    - model: "claude-sonnet-4-20250514"
      provider: "anthropic"
      task_types: ["code", "analysis"]
      fallback_models: ["gpt-4o", "deepseek-chat"]

    - model: "gpt-4o-mini"
      provider: "openai"
      task_types: ["chat", "summary"]
      fallback_models: ["gemini-2.0-flash"]

  model_windows:
    "gpt-4o": 128000
    "claude-sonnet-4-20250514": 200000
    "gpt-4o-mini": 128000
    "deepseek-chat": 64000
    "gemini-2.0-flash": 1000000
```

### 配置解读

- `task_type=code` 的请求 → 路由到 `claude-sonnet-4-20250514`
- 若 Anthropic 不可用 → 依次尝试 `gpt-4o`、`deepseek-chat`
- 若均不可用 → 使用全局 `fallback_model`（`gpt-4o-mini`）
- 未匹配任何 route 的请求 → 直接使用 `default_model`（`gpt-4o`）

!!! tip "最佳实践"
    - 为高优先级任务配置多级 fallback_models
    - 全局 fallback_model 应选择高可用、低成本的模型
    - 在 model_windows 中为所有可能用到的模型配置窗口大小
