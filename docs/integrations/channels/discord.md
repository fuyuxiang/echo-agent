# Discord

## 概述

Discord 通道通过 WebSocket Gateway（v10）维持实时连接，并使用 REST API 发送消息。无需公网端点，Bot 主动连接 Discord 网关。

支持服务器（Guild）中的文字频道和 DM 私聊，群组模式下可配置仅在被 @mention 时响应。

!!! tip
    Gateway 连接支持断线自动重连（Resume），在网络波动时可恢复会话而无需重新拉取历史消息。

---

## 配置示例

```yaml
channels:
  discord:
    enabled: true
    token: "MTIzNDU2Nzg5.ABCdef.GHIjklMNOpqrsTUVwxyz0123456"
    allow_from:
      - "1234567890123456789"   # 用户 ID（字符串）
      - "9876543210987654321"
    group_policy: mention               # open | mention
    reactions_enabled: true
```

| 字段 | 必填 | 说明 |
|------|------|------|
| `token` | 是 | Bot Token，从 Discord Developer Portal 获取 |
| `allow_from` | 否 | 白名单用户 ID 列表，为空则不限制 |
| `group_policy` | 否 | 服务器频道响应策略，默认 `mention` |
| `reactions_enabled` | 否 | 是否启用 reaction 回复，默认 `true` |

---

## 凭证获取

1. 访问 [Discord Developer Portal](https://discord.com/developers/applications)。
2. 点击 **New Application**，输入应用名称。
3. 进入左侧 **Bot** 页面，点击 **Add Bot**。
4. 点击 **Reset Token** 获取 Bot Token。
5. 在 Bot 设置页面，启用以下 **Privileged Gateway Intents**：
   - `GUILDS`
   - `GUILD_MESSAGES`
   - `MESSAGE_CONTENT`（必须，否则无法读取消息内容）

!!! warning
    `MESSAGE_CONTENT` Intent 是特权 Intent。Bot 加入超过 100 个服务器时，需要向 Discord 申请验证。小规模使用直接在 Portal 开启即可。

### 邀请 Bot 到服务器

使用以下 URL 格式邀请 Bot（替换 `CLIENT_ID`）：

```
https://discord.com/api/oauth2/authorize?client_id=CLIENT_ID&permissions=2147551232&scope=bot
```

所需权限位（`permissions=2147551232`）包含：

- Send Messages
- Send Messages in Threads
- Add Reactions
- Read Message History
- Manage Messages（用于编辑自身消息）

!!! tip
    将 Token 通过环境变量注入，避免硬编码：
    ```yaml
    token: "${DISCORD_BOT_TOKEN}"
    ```

---

## 回调/Webhook 设置

本通道使用 **WebSocket Gateway** 连接，无需配置 HTTP Webhook。

Bot 启动时通过 Gateway 建立 WebSocket 连接，接收事件推送。连接流程：

1. 发送 `IDENTIFY` 携带 Token 和 Intents。
2. 接收 `READY` 事件，获取 `session_id` 和 `resume_gateway_url`。
3. 按 `heartbeat_interval` 定时发送心跳。
4. 断线后使用 `RESUME` 恢复，避免消息丢失。

收到 `INVALID SESSION`（op 9）时，通道清空已保存的 `session_id` 并关闭连接；由于 `session_id` 已失效，重连时自动走 `IDENTIFY` 而非 `RESUME`。`RECONNECT`（op 7）则保留 `session_id`，重连后继续用 `RESUME` 恢复。

---

## 能力矩阵

| 能力 | 支持 | 说明 |
|------|------|------|
| 编辑消息 | ✅ | 通过 REST `PATCH /channels/{id}/messages/{id}` |
| Reactions | ✅ | 使用 `PUT /channels/{id}/messages/{id}/reactions` |
| 文件发送 | ❌ | 当前未实现 |
| 实时通信 | ✅ | WebSocket Gateway，延迟极低 |
| 群组支持 | ✅ | 服务器文字频道 |
| 消息分块 | ✅ | 超过 2000 字符自动拆分 |

---

## 常见问题

### Bot 上线但收不到消息？

最常见原因是未启用 `MESSAGE_CONTENT` Intent：

1. 前往 Developer Portal → 应用 → Bot 页面。
2. 开启 **MESSAGE CONTENT INTENT** 开关。
3. 重启 Bot。

如果仍不工作，检查 `allow_from` 是否限制了发送者。

### 频繁断线重连？

检查以下情况：

- 网络不稳定：Gateway 要求持续 WebSocket 连接。
- 心跳超时：确保 heartbeat 按 Discord 返回的 `heartbeat_interval` 发送。
- Rate limit：连续重连过快会被 Discord 限制，当前实现有 300 秒退避机制。

!!! warning
    如果短时间内重连次数过多，Discord 可能临时封禁 Bot 的 Gateway 访问。退避策略（300s）正是为了避免此问题。

### 消息发送返回 429 Too Many Requests？

Discord REST API 有严格的速率限制：

- 每个频道：约 5 条/5 秒
- 全局：约 50 请求/秒

Bot 内置了 rate-limit 响应头解析和自动等待。如果持续触发，可能需要降低消息发送频率。

### 群组中如何仅响应特定频道？

通道提供的过滤维度是 `allow_from`——**用户 ID** 白名单，留空表示不限制。

!!! warning "不支持按频道过滤"
    没有频道或服务器维度的过滤配置。`allow_from` 只比对发送者 ID，把频道 ID 填进去不会限制频道范围，反而会让所有真实用户都匹配失败、通道彻底静默。

    要限制机器人的活动范围，用 Discord 自身的权限体系：只把机器人加入需要的频道，或在频道权限中移除其读取消息的权限。配合 `group_policy: mention`（默认值）可进一步要求群聊中必须 @ 机器人才响应。
