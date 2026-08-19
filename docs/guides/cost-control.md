# 成本控制

Echo Agent 内置成本追踪与预算控制系统，帮助你了解各模型的实际开销并避免意外超支。

## 概述

成本系统涵盖以下能力：

- 按模型归因计费（per-model cost attribution）
- 每日预算上限与软阈值预警
- 自定义定价表（适配本地/私有化部署模型）
- CLI 与 Dashboard 双入口查询
- 路由器成本感知决策集成

---

## 配置

在项目配置文件中启用成本追踪：

```yaml
cost:
  enabled: true
  daily_budget_usd: 5.0
  soft_threshold_ratio: 0.8
  pricing_overrides:
    my-local-llama:
      input_per_1k: 0.0
      output_per_1k: 0.0
    custom-gpt4:
      input_per_1k: 0.03
      output_per_1k: 0.06
```

### 字段说明

| 字段 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `enabled` | bool | `false` | 是否启用成本追踪与预算控制 |
| `daily_budget_usd` | float | `0.0` | 每日预算（美元），0 表示不限制 |
| `soft_threshold_ratio` | float | `0.8` | 软预警触发比例（相对于 daily_budget_usd） |
| `pricing_overrides` | dict | `{}` | 模型定价覆盖表 |

---

## 每日预算与软预警

当 `daily_budget_usd` 设置为正数时，系统会在每个 UTC 自然日内累计消耗。

- **软预警**：当日消耗达到 `daily_budget_usd * soft_threshold_ratio` 时触发告警日志。例如预算 $5、阈值 0.8，则在消耗 $4 时发出预警。
- **硬上限**：当日消耗达到 `daily_budget_usd` 时触发预算耗尽行为（见下文）。

!!! question "需维护者确认"
    当每日预算耗尽时，系统行为是硬停止（拒绝新请求）还是服务降级（fallback 到更廉价模型）？此行为尚待确认。

---

## 按模型成本归因

每次 LLM 调用的 token 用量通过 `LLMResponse.usage` 追踪：

- `input_tokens` — 输入 token 数
- `output_tokens` — 输出 token 数
- `cache_read_input_tokens` — 命中缓存的输入 token 数

系统根据各模型的定价表（内置或 `pricing_overrides`）计算每次调用的成本，并按模型维度汇总。

---

## 定价覆盖

对于自托管模型或自定义端点，使用 `pricing_overrides` 设置实际单价：

```yaml
cost:
  pricing_overrides:
    # 本地模型不计费
    ollama-llama3:
      input_per_1k: 0.0
      output_per_1k: 0.0
    # 自定义价格
    azure-gpt4o:
      input_per_1k: 0.005
      output_per_1k: 0.015
```

未在覆盖表中出现的模型将使用内置定价数据。

---

## CLI 查询

使用 `echo-agent cost` 命令查看成本报告：

```bash
# 查看最近 7 天成本
echo-agent cost --days 7

# 输出 JSON 格式（适合脚本处理）
echo-agent cost --days 30 --json
```

输出内容包含：

- 每日总消耗
- 按模型分组的成本明细
- 预算使用百分比
- 缓存命中率统计

---

## Dashboard 分析页

Dashboard 的 Analytics 页面提供可视化成本视图：

- 日/周/月成本趋势图
- 模型成本占比饼图
- 缓存命中率趋势
- 预算消耗进度条

---

## 成本优化策略

### 1. 模型路由

路由器支持成本感知决策 —— 简单任务自动分配到低成本模型：

```yaml
router:
  strategy: cost_aware
  rules:
    - condition: "complexity == 'simple'"
      prefer: cheap_model
    - condition: "complexity == 'complex'"
      prefer: capable_model
```

### 2. 提示缓存

利用 prompt caching 减少重复输入 token 的计费：

- 系统提示、工具定义等静态内容会被缓存
- 通过 `cache_read_input_tokens` 指标监控缓存命中率
- 缓存命中的 token 通常以大幅折扣计费

### 3. 凭证池负载均衡

当配置多个 API Key 时，系统自动进行负载均衡，避免单 Key 限流导致的重试开销。

### 4. 上下文窗口管理

- 控制对话历史长度，避免不必要的 token 堆积
- 使用摘要而非完整历史进行长期对话
- 合理设置 `max_tokens` 限制输出长度

---

## 预算告警行为

| 状态 | 行为 |
|------|------|
| 消耗 < 软阈值 | 正常运行 |
| 消耗 >= 软阈值 | 日志 WARNING，Dashboard 黄色提示 |
| 消耗 >= 硬上限 | 见下方说明 |

!!! warning "预算耗尽"
    当日消耗达到 `daily_budget_usd` 时，系统将阻止新的 LLM 调用。请确保预算设置留有适当余量，或在非关键场景使用 `0`（不限制）。

---

## 完整配置示例

```yaml
cost:
  enabled: true
  daily_budget_usd: 10.0
  soft_threshold_ratio: 0.75
  pricing_overrides:
    local-llama3-70b:
      input_per_1k: 0.0
      output_per_1k: 0.0
    deepseek-chat:
      input_per_1k: 0.001
      output_per_1k: 0.002

router:
  strategy: cost_aware
```
