# Cron Channel

Cron 通道通过定时任务触发 Agent 执行，无需外部消息输入。

---

## 概述

Cron 通道不是传统的"聊天"通道，而是一个事件注入机制。它按计划时间向 Agent 发送预定义消息，触发自动化工作流。

## 配置

```yaml
channels:
  cron:
    enabled: true
```

## 能力

| 能力 | 支持 |
|------|------|
| 编辑消息 | ❌ |
| 表情回应 | ❌ |
| 文件发送 | ❌ |
| 实时响应 | ❌ |
| 群聊 | ❌ |

## 创建定时任务

### 通过 CLI

```bash
echo-agent cron list
echo-agent cron authorize <job-id>
echo-agent cron revoke <job-id>
```

### 通过 Agent 对话

Agent 可以通过 `cronjob` 工具自行创建定时任务：

> "每天早上 9 点给我发送天气预报"

### 通过 Dashboard

Dashboard Cron 页面支持可视化管理定时任务。

## 授权机制

新创建的 Cron 任务默认需要授权才能执行。这是安全设计：

- 防止 Agent 在无人值守时创建并执行高风险定时任务
- `echo-agent cron authorize <id>` 明确授权
- 已授权任务修改后是否需要重新授权，取决于具体实现

## 输出路由

Cron 通道本身没有"发送"能力，投递目标记在**每个任务**上，不是全局配置 —— 配置中没有 `gateway.deliveryRoutes` 这样的路由表。

任务载荷中的两个字段决定输出去向：

| 字段 | 说明 |
|------|------|
| `deliver_channel` | 目标通道名，如 `telegram` |
| `deliver_chat_id` | 目标会话 ID |

两者留空时回落到任务的 `channel` / `chat_id`。用 `cronjob` 工具或 Dashboard 创建任务时指定即可。

## 常见问题

**任务创建了但不执行？**
- 检查是否已授权：`echo-agent cron list`
- 确认 Gateway 正在运行

**时区问题？**
- Cron 表达式使用系统本地时区
