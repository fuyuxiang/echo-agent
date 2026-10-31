# 定时任务

**适用范围：独立 Python Runtime。** 桌面端操作见 [Desktop 使用指南](../desktop/index.md)。

Echo Agent 内置定时任务系统，允许通过 cronjob 工具创建、管理和监控周期性任务。本指南涵盖调度系统的完整使用方式。

## 系统概述

定时任务系统由以下组件构成：

- **cronjob 工具** — 创建和管理定时任务的核心工具
- **Scheduler（调度器）** — 负责按 cron 表达式触发任务执行
- **Cron Channel（定时通道）** — 专用通道，承载定时任务的输出与状态
- **Dashboard Cron 页面** — 可视化管理界面

## 调度器配置

调度器通过 `SchedulerConfig` 进行全局配置：

```yaml
scheduler:
  enabled: true
  max_concurrent_jobs: 10
```

| 参数 | 默认值 | 说明 |
|------|--------|------|
| `enabled` | `true` | 是否启用定时任务系统 |
| `max_concurrent_jobs` | `10` | 覆盖完整执行周期的并发上限；手动超限返回冲突，周期触发稍后再检查 |

## 创建定时任务

使用 cronjob 工具创建任务：

```yaml
tool: cronjob
action: create
name: "daily-report"
schedule: "0 9 * * *"
command: "生成每日汇总报告并发送到通知通道"
```

Cron 表达式按服务器本地时区解释；部署需固定时区并检查下次执行时间。普通五字段表达式的日/月会周期重复，不能当作一次性日期提醒。

### Cron 表达式语法

采用标准五位 cron 格式：

```
┌───────────── 分钟 (0-59)
│ ┌───────────── 小时 (0-23)
│ │ ┌───────────── 日 (1-31)
│ │ │ ┌───────────── 月 (1-12)
│ │ │ │ ┌───────────── 星期 (0-6, 0=周日)
│ │ │ │ │
* * * * *
```

常用示例：

| 表达式 | 含义 |
|--------|------|
| `0 9 * * *` | 每天 9:00 |
| `*/15 * * * *` | 每 15 分钟 |
| `0 0 * * 1` | 每周一 00:00 |
| `0 8 1 * *` | 每月 1 日 8:00 |
| `0 */2 * * *` | 每 2 小时 |

## 授权模型

!!! danger "安全警告"
    cronjob 工具的风险等级为 `dangerous`。创建新的定时任务需要明确的授权审批。

### 为什么是 dangerous 级别

定时任务会在无人值守的情况下周期性执行，可能：

- 消耗大量系统资源
- 执行敏感操作
- 产生不可预期的副作用

### 审批流程

交互式 `cronjob create/authorize` 必须经过危险工具的人类批准；批准绑定展示的指令、计划与投递目标。管理员也可通过 Dashboard/REST 明确设置 `authorize_unattended: true`。`cron_authorized` 是调度器从已签发授权派生的内部信号，不是调用者可以填入上下文来取得授权的入口。

授权按**单个任务**显式授予，没有通道级的自动授权规则——新建的定时任务默认未授权，不会因为属于 cron 通道就自动获得执行许可。

这样设计是为了让「定时」与「许可」分离：任务的调度配置可以随时改动，而它能否在无人看管时动手做事，需要一次明确的人工确认。

#### 给已存在的任务补授权

授权与任务内容绑定，**修改指令、频率或投递目标都会使已有授权失效**，因此「重新授权」是常规操作，而不是异常情况。共有三条路径：

| 路径 | 服务运行中可用 | 说明 |
|------|:---:|------|
| 在对话里说「授权定时任务 `<job_id>`」 | ✅ | agent 调用 `cronjob(action="authorize")`，会先列出该任务的指令/频率/投递目标请你确认 |
| Dashboard 定时任务页勾选授权 | ✅ | 等价于 REST `PUT /cron/{id}` 带 `authorize_unattended: true` |
| `echo-agent cron authorize <job_id>` | ❌ | 仅在**服务已停止**时可用 |

CLI 那条路径受实例锁保护：gateway 运行时它会直接拒绝，因为离线改动会被运行中的实例覆盖。所以服务正常运行期间，应使用对话或 Dashboard 授权。

```bash
# 以下命令需先停止服务
echo-agent cron list                  # 查看任务及其授权状态
echo-agent cron authorize <job_id>    # 授权指定任务
echo-agent cron revoke <job_id>       # 撤销授权
```

对话里同样可以撤销：说「撤销定时任务 `<job_id>` 的授权」即可（对应 `cronjob(action="revoke")`）。

### 无人值守模式

当 `unattended=true` 时，审批流程有所不同：

- 已授权的任务可按许可执行 WRITE/EXEC 操作
- 创建新定时任务等 DANGEROUS 操作仍拒绝；无人值守任务不能自我扩权或挂起等待无人处理的审批

## Cron 通道

定时任务通过内部 CRON 事件进入同一个 Runtime。指定来源会话或投递目标时，结果发往该会话；不能解析目标时会使用 `cron:<job_id>` 上下文，明确记录“无法投递”，不会把丢弃输出算作成功。

每次触发先持久化唯一 `run_id`，再入队。`queued` 只代表入队；`completed` 表示回合完成且投递处理成功，仍须结合渠道回执能力判断对端确认。预算耗尽、模型错误后的不完整回答记为 `incomplete`，投递失败记为 `error`。历史保留最近 100 次运行，迟到回写只影响它对应的运行，不能覆盖较新的结果。

同一任务在本次运行尚未结束时不会再次触发，包括手动触发。重启时，上一个进程留下的排队/执行记录记为 `interrupted`，不会盲目重新执行可能已产生副作用的操作；检查结果和外部系统后再决定是否手动重试。定时计划会从当前时间重新计算，不补跑所有错过的周期。

### 投递恢复与日报历史

微信缺少发送上下文或明确拒收时，已生成的结果记为 `delivery_pending`，释放本次执行槽位，保留原报告。
新的微信消息恢复会话后，Runtime 自动继续投递；成功后记录 `delivery_recovered` 和恢复时间。
补发不会重新运行任务，也不会重发已经确认的分段。发送结果未知时须在 Dashboard 中先核对。

定时任务可以通过 payload 中的 `report_history_path` 指定工作区内的相对 JSON 路径，
例如 `data/ai-radar/history.json`。Runtime 按执行日期维护 `reports` 数组的投递字段：
生成后为 `pending_delivery`，收到完整投递回执后才写入 `status: success` 和实际 `pushed_at`。
`job_id`、`run_id` 和 `delivery_status` 用于关联调度器的执行记录。
检索内容、来源和技术条目仍由任务维护；`last_pushed` 在投递失败时会撤回当天的提前成功标记。
任务应以 Runtime 回执为送达依据，不能自行提前填写成功。该路径的变更会使现有任务授权失效，需要重新授权。

## Dashboard Cron 页面

Dashboard 提供专门的 Cron 管理页面，支持：

- 查看所有定时任务列表及状态
- 手动触发任务执行
- 暂停/恢复任务
- 查看任务执行历史与日志
- 删除任务

## 管理任务

### 列出任务

```yaml
tool: cronjob
action: list
```

### 暂停与恢复

在 Dashboard Cron 页操作，或使用管理员 `PUT {api_prefix}/cron/{id}` 更新 `enabled`。`cronjob` 的动作只有 create/list/delete/trigger/authorize/revoke，没有 pause/resume。暂停不撤销已执行的外部副作用；在飞执行与新触发冲突应检查 run 历史。

### 删除任务

```yaml
tool: cronjob
action: delete
job_id: "<id returned by create/list>"
```

## 使用场景示例

### 每日报告生成

```yaml
tool: cronjob
action: create
name: "daily-summary"
schedule: "0 9 * * *"
command: "汇总过去 24 小时的通道活动，生成摘要报告"
```

### 定期清理

```yaml
tool: cronjob
action: create
name: "weekly-cleanup"
schedule: "0 3 * * 0"
command: "清理超过 30 天的临时文件和过期缓存"
```

### 健康检查

```yaml
tool: cronjob
action: create
name: "health-check"
schedule: "*/30 * * * *"
command: "检查所有后端服务连通性，异常时发送告警"
```

### 数据同步

```yaml
tool: cronjob
action: create
name: "sync-external-data"
schedule: "0 */4 * * *"
command: "从外部 API 同步最新数据到本地存储"
```

## 安全建议

!!! warning "最小权限原则"
    定时任务应仅授予完成其功能所需的最小权限。避免创建拥有广泛权限的定时任务。

- 定期审查活跃的定时任务列表
- 为敏感操作的定时任务设置执行时间窗口
- 监控 `max_concurrent_jobs` 使用情况，防止资源耗尽
- 在生产环境中谨慎使用 `cron_authorized` 预授权标志
