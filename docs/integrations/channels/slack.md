# Slack

## 概述

Slack 通道通过 Socket Mode（WebSocket）接收事件，使用 Web API 发送消息。Socket Mode 无需公网端点，Bot 主动连接 Slack 的 WebSocket 服务器。

支持频道消息和线程（Thread）回复，天然适配 Slack 的对话模型。

!!! tip
    Socket Mode 是 Slack 推荐的开发/内部部署方式，无需配置 Ingress 或 SSL 证书。适合企业内网环境。

---

## 配置示例

```yaml
channels:
  - type: slack
    bot_token: "xoxb-your-bot-token"
    app_token: "xapp-your-app-token"
    allow_from:
      - "U01ABCDEF23"      # Slack 用户 ID
      - "U04XYZABC56"
    reactions_enabled: true
```

| 字段 | 必填 | 说明 |
|------|------|------|
| `bot_token` | 是 | Bot User OAuth Token（`xoxb-` 前缀） |
| `app_token` | 是 | App-Level Token（`xapp-` 前缀），用于 Socket Mode |
| `allow_from` | 否 | 白名单用户 ID 列表，为空则不限制 |
| `reactions_enabled` | 否 | 是否启用 reaction 回复，默认 `true` |

---

## 凭证获取

### 第一步：创建 Slack App

1. 访问 [api.slack.com/apps](https://api.slack.com/apps)，点击 **Create New App**。
2. 选择 **From scratch**，输入应用名称并选择 Workspace。

### 第二步：启用 Socket Mode

1. 左侧菜单进入 **Socket Mode**。
2. 开启 **Enable Socket Mode**。
3. 系统提示创建 App-Level Token，名称随意（如 `echo-agent`），Scope 选择 `connections:write`。
4. 记录生成的 `xapp-...` Token。

### 第三步：配置 Bot Token Scopes

进入 **OAuth & Permissions** → **Scopes** → **Bot Token Scopes**，添加：

| Scope | 用途 |
|-------|------|
| `chat:write` | 发送消息 |
| `chat:write.customize` | 自定义发送者名称/头像 |
| `reactions:write` | 添加 Reaction |
| `reactions:read` | 读取 Reaction 事件 |
| `channels:history` | 读取公共频道消息 |
| `groups:history` | 读取私有频道消息 |
| `im:history` | 读取 DM 消息 |
| `mpim:history` | 读取群组 DM 消息 |

### 第四步：订阅事件

进入 **Event Subscriptions** → 开启 → **Subscribe to bot events**：

- `message.channels`
- `message.groups`
- `message.im`
- `message.mpim`
- `reaction_added`（如需 reaction 触发）

### 第五步：安装到 Workspace

进入 **Install App**，点击 **Install to Workspace** 并授权。记录生成的 `xoxb-...` Token。

!!! warning
    `bot_token`（xoxb-）和 `app_token`（xapp-）是两个不同的凭证，缺一不可。建议通过环境变量管理：
    ```yaml
    bot_token: "${SLACK_BOT_TOKEN}"
    app_token: "${SLACK_APP_TOKEN}"
    ```

---

## 回调/Webhook 设置

本通道使用 **Socket Mode**，无需配置公网 Webhook URL。

Bot 启动时通过 `apps.connections.open` API 获取 WebSocket URL，然后建立连接接收事件。

事件交互流程：

1. 收到 `events_api` 类型的 envelope。
2. 发送 `acknowledge` 响应（必须在 3 秒内）。
3. 处理消息并通过 Web API 回复。

!!! question "需维护者确认"
    Socket Mode 连接断开后的重连策略是什么？是否有指数退避？

---

## 能力矩阵

| 能力 | 支持 | 说明 |
|------|------|------|
| 编辑消息 | ✅ | 通过 `chat.update` API |
| Reactions | ✅ | 通过 `reactions.add` API |
| 文件发送 | ❌ | 当前未实现 |
| 实时通信 | ✅ | Socket Mode WebSocket |
| 群组/线程支持 | ✅ | 通过 `thread_ts` 实现线程回复 |
| 消息分块 | ✅ | Slack 消息上限 40000 字符（通常无需分块） |

---

## 常见问题

### Bot 连接成功但收不到消息？

1. 确认已订阅正确的事件（`message.channels` 等）。
2. 确认 Bot 已被邀请到目标频道（`/invite @bot-name`）。
3. 检查 `allow_from` 是否限制了发送者 ID。

!!! tip
    Slack 的 Bot 不会自动加入频道。必须在频道中执行 `/invite @bot-name` 或通过 API 加入。

### Thread 回复没有正确关联？

Bot 使用消息的 `thread_ts` 字段进行线程回复。确保：

- 收到的消息包含 `thread_ts` 时，回复带上同一 `thread_ts`。
- 新消息（非线程内）的回复会创建新线程。

### App Token 权限不足？

App-Level Token 只需要 `connections:write` scope。如果连接失败提示权限不足：

1. 前往 **Basic Information** → **App-Level Tokens**。
2. 检查或重新生成 Token，确保包含 `connections:write`。

### 如何区分公共频道和私聊消息？

通过事件中的 `channel_type` 字段：

- `channel`: 公共频道
- `group`: 私有频道
- `im`: 单人 DM
- `mpim`: 多人 DM

当前实现统一处理所有类型，通过 `allow_from` 做访问控制。
