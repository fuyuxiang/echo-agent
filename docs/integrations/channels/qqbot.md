# QQ 机器人（QQ Bot）

## 概述

QQ Bot 通道通过**官方 QQ Bot API v2** 接入，采用 WebSocket 网关接收消息事件 + REST API 发送消息的混合架构。支持 C2C（单聊）、群聊和频道三种消息场景。

通道具备原生 Markdown 支持（带自动降级）、富媒体发送（图片/视频/音频/文件）、消息去重等特性，适合构建功能丰富的 QQ 机器人。

!!! tip
    QQ Bot API 的 Markdown 消息支持需要额外申请权限。通道内置了自动降级逻辑：发送失败时回退到纯文本，并缓存拒绝状态 24 小时后重新探测。

---

## 配置示例

```yaml
channels:
  qqbot:
    enabled: true
    app_id: "your-app-id"
    app_secret: "your-app-secret"
    sandbox: false
    markdown_support: true
    media_enabled: true
    media_max_file_size_mb: 20
    media_upload_cache_size: 500
    media_parse_tags: true
    allow_from:
      - "user_openid_1"
      - "user_openid_2"
```

| 字段 | 必填 | 默认值 | 说明 |
|------|------|--------|------|
| `app_id` | 是 | — | QQ Bot 应用 ID |
| `app_secret` | 是 | — | QQ Bot 应用密钥 |
| `sandbox` | 否 | `false` | 是否使用沙箱环境进行测试 |
| `markdown_support` | 否 | `true` | 是否启用 Markdown 消息发送 |
| `media_enabled` | 否 | `true` | 是否启用富媒体发送 |
| `media_max_file_size_mb` | 否 | `20` | 媒体文件最大上传大小（MB） |
| `media_upload_cache_size` | 否 | `500` | 媒体上传缓存条目数（上传一次，复用 file_info） |
| `media_parse_tags` | 否 | `true` | 是否解析接收消息中的附件标签 |
| `allow_from` | 否 | `[]` | 允许交互的用户 openid 白名单，为空不限制 |

---

## 凭证获取

1. 访问 [QQ 开放平台](https://q.qq.com/) 并注册开发者账号
2. 进入「机器人」→「创建机器人」
3. 完成机器人基本信息填写，获取 **AppID** 和 **AppSecret**
4. 在「开发设置」中配置消息接收意图（Intents）：
   - `PUBLIC_GUILD_MESSAGES`（频道公域消息）
   - `GUILD_MESSAGES`（频道私域消息）
   - `GROUP_AT_MESSAGE_CREATE`（群聊 @消息）
   - `C2C_MESSAGE_CREATE`（单聊消息）
5. 申请消息接收权限（需审核通过）

!!! warning
    Markdown 消息、富媒体消息等高级功能需要单独申请权限。未获批时通道会自动降级到纯文本模式。

---

## 回调 / Webhook 设置

QQ Bot API v2 使用 **WebSocket 网关**接收事件，无需配置公网回调 URL。

连接流程：

1. 使用 AppID + AppSecret 调用 `getAppAccessToken` 获取 access_token
2. 调用 `/gateway` 接口获取 WebSocket 网关地址
3. 建立 WebSocket 连接并发送鉴权信息
4. 通过心跳保持连接活跃
5. 接收事件推送（消息、交互等）

```yaml
# 网关连接鉴权 payload
{
  "op": 2,
  "d": {
    "token": "QQBot {access_token}",
    "intents": 0 | INTENT_FLAGS,
    "shard": [0, 1]
  }
}
```

!!! tip
    沙箱模式（`sandbox: true`）连接到测试环境网关，适合开发调试。正式上线前切换为 `false`。

---

## 能力矩阵

| 能力 | 支持情况 | 备注 |
|------|----------|------|
| 消息编辑 | ❌ | QQ Bot API 不支持修改已发消息 |
| 表情回应 | ❌ | 不支持 |
| 文件发送 | ✅（条件） | 需 `media_enabled: true`，通过富媒体 API |
| 实时消息 | ✅ | WebSocket 网关推送 |
| 群聊 | ✅ | 支持 C2C / 群聊 / 频道 |
| Markdown | ✅（降级） | 自动探测，被拒后 24h 回退纯文本 |

**消息类型（msg_type）：**

| msg_type | 说明 |
|----------|------|
| `0` | 纯文本 |
| `2` | Markdown |
| `7` | 富媒体（图片/视频/音频/文件） |

---

## Markdown 降级机制

通道采用「先尝试后降级」策略：

1. 首次发送使用 `msg_type=2`（Markdown）
2. 如果 API 返回错误包含「不允许」或「无权限」关键字
3. 自动回退到 `msg_type=0`（纯文本）重新发送
4. 缓存拒绝状态，后续 24 小时内直接使用纯文本
5. 24 小时后重新探测 Markdown 可用性

!!! warning
    Markdown 权限可能在审核期间被临时收回。降级机制确保消息不丢失，但格式可能简化。

---

## 富媒体发送

当 `media_enabled: true` 时，支持通过 QQ 富媒体 API（`msg_type=7`）发送：

- 图片（image）
- 视频（video）
- 音频（audio）
- 文件（file）

**缓存机制：** 媒体文件上传后，`file_info` 会被缓存（容量由 `media_upload_cache_size` 控制）。相同文件重复发送时直接复用缓存，避免重复上传。

**附件解析：** 当 `media_parse_tags: true` 时，接收到的消息中的图片/视频/音频/文件附件会被自动解析提取。

---

## 消息去重

通道使用 `OrderedDict` 实现消息去重，防止 WebSocket 重连或网关重发导致的重复处理。最大消息长度限制为 4000 字符。

---

## 常见问题

!!! question "需维护者确认"
    沙箱环境的消息接收意图配置是否与正式环境完全一致？

**Q: Markdown 消息一直降级为纯文本？**

1. 确认已在 QQ 开放平台申请并通过 Markdown 消息权限
2. 检查是否在 24 小时缓存窗口内（可重启服务清除缓存）
3. 沙箱环境可能不支持 Markdown，切换到正式环境测试

**Q: 媒体文件发送失败？**

- 确认 `media_enabled: true`
- 检查文件大小是否超过 `media_max_file_size_mb`（默认 20MB）
- 确认文件类型为支持的格式（image/video/audio/file）
- 检查富媒体 API 权限是否已申请

**Q: 收不到群聊消息？**

- 确认已配置 `GROUP_AT_MESSAGE_CREATE` 意图
- 群聊中需要 @机器人 才能触发
- 检查 `allow_from` 白名单设置
- 确认消息接收权限已审核通过

**Q: WebSocket 连接频繁断开？**

- 检查心跳间隔是否正确（按网关下发的 `heartbeat_interval` 发送）
- 确认 access_token 未过期（有效期 2 小时，需自动刷新）
- 网络不稳定时通道会自动重连，检查日志确认重连状态
