# 定时任务

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
| `max_concurrent_jobs` | `10` | 最大并发任务数，超出时排队等待 |

## 创建定时任务

使用 cronjob 工具创建任务：

```yaml
tool: cronjob
action: create
name: "daily-report"
schedule: "0 9 * * *"
task: "生成每日汇总报告并发送到通知通道"
```

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

创建新任务需要满足以下条件之一：

1. **人工审批**（`approval_source="human"`）— 维护者在 Dashboard 或交互中确认
2. **预授权标志**（`cron_authorized=true`）— 在 `ToolExecutionContext` 中设置

```yaml
# ToolExecutionContext 示例
context:
  cron_authorized: true
  unattended: false
```

!!! question "需维护者确认"
    Cron Channel 的授权流转细节（如通道级别的自动授权规则）需要维护者确认具体实现方式。

### 无人值守模式

当 `unattended=true` 时，审批流程有所不同：

- 若同时设置 `cron_authorized=true`，任务可自动创建
- 若未设置 `cron_authorized`，任务创建将被拒绝（不会挂起等待人工审批）

## Cron 通道

Cron 通道是定时任务的专用执行环境：

- 每个定时任务绑定到一个 cron 通道
- 任务输出和状态信息写入该通道
- 通道提供任务执行的隔离上下文

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

### 暂停任务

```yaml
tool: cronjob
action: pause
name: "daily-report"
```

### 恢复任务

```yaml
tool: cronjob
action: resume
name: "daily-report"
```

### 删除任务

```yaml
tool: cronjob
action: delete
name: "daily-report"
```

## 使用场景示例

### 每日报告生成

```yaml
tool: cronjob
action: create
name: "daily-summary"
schedule: "0 9 * * *"
task: "汇总过去 24 小时的通道活动，生成摘要报告"
```

### 定期清理

```yaml
tool: cronjob
action: create
name: "weekly-cleanup"
schedule: "0 3 * * 0"
task: "清理超过 30 天的临时文件和过期缓存"
```

### 健康检查

```yaml
tool: cronjob
action: create
name: "health-check"
schedule: "*/30 * * * *"
task: "检查所有后端服务连通性，异常时发送告警"
```

### 数据同步

```yaml
tool: cronjob
action: create
name: "sync-external-data"
schedule: "0 */4 * * *"
task: "从外部 API 同步最新数据到本地存储"
```

## 安全建议

!!! warning "最小权限原则"
    定时任务应仅授予完成其功能所需的最小权限。避免创建拥有广泛权限的定时任务。

- 定期审查活跃的定时任务列表
- 为敏感操作的定时任务设置执行时间窗口
- 监控 `max_concurrent_jobs` 使用情况，防止资源耗尽
- 在生产环境中谨慎使用 `cron_authorized` 预授权标志
