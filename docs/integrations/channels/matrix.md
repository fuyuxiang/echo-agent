# Matrix 通道

## 概述

Matrix 通道通过长轮询 `/sync` 端点接收消息，通过 REST API 发送回复。无需公网端点，适合部署在内网或防火墙后。Matrix 是去中心化的开放通信协议，支持自建 Homeserver（如 Synapse、Dendrite）。

本通道支持实时流式输出（`is_realtime=True`），Agent 生成的 token 可增量推送到房间。

## 配置示例

```yaml
channels:
  matrix:
    enabled: true
    homeserver: https://matrix.example.com
    user_id: "@echo-bot:example.com"
    access_token: ${MATRIX_ACCESS_TOKEN}
    allow_rooms:
      - "!abcdef123456:example.com"
      - "!support-room:example.com"
```

| 字段 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `homeserver` | string | — | Homeserver URL（含协议） |
| `user_id` | string | — | Bot 的完整 Matrix ID |
| `access_token` | string | — | Bearer 访问令牌 |
| `allow_rooms` | list | [] | 房间 ID 白名单，为空则响应所有已加入房间 |

## 凭证获取

### 创建 Bot 账号

1. 在 Homeserver 上注册一个新用户作为 Bot：

```bash
# 使用 Synapse admin API 注册（需管理员权限）
curl -X POST "https://matrix.example.com/_synapse/admin/v1/register" \
  -H "Authorization: Bearer <admin_token>" \
  -H "Content-Type: application/json" \
  -d '{"nonce": "...", "username": "echo-bot", "password": "secure-password", "admin": false}'
```

2. 或者通过标准客户端注册流程创建账号

### 获取 Access Token

**方式一：通过 Login API**

```bash
curl -X POST "https://matrix.example.com/_matrix/client/v3/login" \
  -H "Content-Type: application/json" \
  -d '{
    "type": "m.login.password",
    "identifier": {"type": "m.id.user", "user": "echo-bot"},
    "password": "secure-password"
  }'
```

响应中的 `access_token` 字段即为所需令牌。

**方式二：通过管理后台**

部分 Homeserver 管理面板（如 Synapse Admin UI）可直接为用户生成长期令牌。

!!! warning "令牌安全"
    Access Token 等同于完整账户权限。请妥善保管，不要提交到版本控制，建议通过环境变量注入。

### 邀请 Bot 加入房间

```bash
# 在目标房间中邀请 Bot
curl -X POST "https://matrix.example.com/_matrix/client/v3/rooms/!roomid:example.com/invite" \
  -H "Authorization: Bearer <your_token>" \
  -H "Content-Type: application/json" \
  -d '{"user_id": "@echo-bot:example.com"}'
```

Bot 启动后会自动接受邀请（auto-join）。

## 能力矩阵

| 能力 | 支持 | 说明 |
|------|------|------|
| 编辑已发消息 | ❌ | 当前版本未实现 `m.replace` |
| 表情回应 | ✅ | 支持 `m.reaction` 事件 |
| 文件附件 | ❌ | 不处理媒体消息 |
| 实时流式输出 | ✅ | 增量编辑消息实现流式效果 |
| 群组/多人会话 | ✅ | 基于房间的天然多人支持 |
| 语音消息 | ✅ | 支持接收语音消息事件 |
| 投票 | ✅ | 支持 Matrix 投票事件 |

## 内部机制

### Sync 检查点持久化

通道使用 Matrix `/sync` 响应中的 `since` token 作为检查点。该 token 持久化到磁盘，确保：

- 进程重启后从上次位置继续同步，不遗漏消息
- 不会重复处理已见事件

### 房间白名单隔离

```yaml
allow_rooms:
  - "!abcdef123456:example.com"
```

!!! tip "allow_rooms 行为"
    - 配置了 `allow_rooms` 时，Bot 仅在列出的房间中响应消息
    - 留空或不配置时，Bot 响应所有已加入房间的消息
    - Bot 仍会加入被邀请的房间，但不在白名单内的房间中不会产生回复

## 常见问题

!!! question "Bot 被邀请到房间但不响应？"
    检查 `allow_rooms` 配置。如果设置了白名单但未包含该房间 ID，Bot 会加入但不回复。将房间 ID 添加到白名单或清空 `allow_rooms` 即可。

!!! question "如何获取房间 ID？"
    在 Element 等客户端中，进入房间设置 → 高级 → 「内部房间 ID」。格式为 `!randomstring:server.name`。

!!! question "Sync 超时或断连怎么办？"
    通道内置重试逻辑，网络中断后会自动重连并从上次 since token 恢复。无需手动干预。

!!! question "需维护者确认"
    是否计划支持 E2EE（端到端加密）房间？当前实现仅处理未加密消息。
