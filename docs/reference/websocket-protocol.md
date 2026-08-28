# WebSocket 协议参考

Echo Agent Gateway 提供会话通信和 Dashboard 推送两个 WebSocket 端点。

| 端点 | 默认路径 | 用途 |
|------|----------|------|
| 会话通信 | `/ws` | 与 Agent 进行文本交互 |
| Dashboard 推送 | `/ws/dashboard` | 订阅任务、定时任务等运行事件 |

会话端点路径可由 `gateway.ws_path` 修改。下文只描述当前服务端实际接受和发出的帧；技能、知识库和其他管理操作使用 REST API。

## 会话 WebSocket

### 建立连接与认证

连接建立后必须在 5 秒内发送 `auth` 帧。令牌可以直接放在该帧中，也可以放在握手请求头或 URL 查询参数中；无论令牌来自哪里，`auth` 帧本身都不能省略。

```json
{
  "type": "auth",
  "token": "your-api-token",
  "platform": "cli",
  "user_id": "alice",
  "chat_id": "alice",
  "session_key": "cli:alice"
}
```

字段说明：

| 字段 | 必填 | 说明 |
|------|------|------|
| `type` | 是 | 固定为 `auth` |
| `token` | 按配置 | API 或 admin token；未配置令牌时可省略 |
| `platform` | 否 | 客户端平台，缺省或未知值归一为 `ws` |
| `user_id` | 按认证模式 | 用户标识 |
| `chat_id` | 否 | 会话投递标识，默认等于 `user_id` |
| `session_key` | 否 | 恢复指定会话；服务端会校验其归属，不能用于访问其他用户的会话 |

也可以通过握手请求携带令牌：

```http
Authorization: Bearer your-api-token
```

或者使用 `gateway.auth.token_header` 配置的请求头。兼容入口 `?token=` 仍可用于认证，但查询参数可能进入访问日志、代理日志和浏览器历史，生产环境应优先使用请求头或 `auth` 帧。

认证成功：

```json
{"type": "auth_ok", "session_key": "cli:alice"}
```

认证失败时服务端发送 `error` 并关闭连接：

```json
{"type": "error", "error": "unauthorized"}
```

### 客户端发送的帧

#### message

发送一条纯文本消息：

```json
{
  "type": "message",
  "text": "总结最近的任务进展",
  "is_group": false
}
```

消息进入队列后，服务端返回事件 ID：

```json
{"type": "accepted", "event_id": "evt_abc123"}
```

`accepted` 只表示已入队，不代表 Agent 已完成处理。空文本不会入队。

#### interrupt

请求停止当前会话正在执行的轮次。建议携带此前 `accepted` 帧中的 `event_id`，避免延迟到达的中断影响下一轮。

```json
{"type": "interrupt", "event_id": "evt_abc123"}
```

中断请求成功入队后返回：

```json
{"type": "accepted"}
```

#### ping

```json
{"type": "ping"}
```

服务端响应：

```json
{"type": "pong"}
```

### 服务端消息帧

Agent 的输出统一使用 `message` 帧。流式片段和最终消息通过 `is_final` 区分；`message_kind` 和 `metadata` 用于表达进度、认知状态、错误等通用出站事件。

```json
{
  "type": "message",
  "event_id": "evt_reply",
  "reply_to_id": "evt_abc123",
  "channel": "gateway:cli",
  "chat_id": "alice",
  "text": "已完成三项任务。",
  "is_final": true,
  "message_kind": "final",
  "edit_message_id": null,
  "metadata": {}
}
```

常见错误帧：

```json
{"type": "error", "error": "authenticate first"}
{"type": "error", "error": "rate limited"}
{"type": "error", "error": "server overloaded"}
{"type": "error", "error": "internal error"}
```

## Dashboard WebSocket

Dashboard 连接 `/ws/dashboard` 后同样必须在 5 秒内发送认证帧。该端点当前只从帧中读取令牌：

```json
{"type": "auth", "token": "your-api-token"}
```

成功后返回 `{"type":"auth_ok"}`。随后可订阅事件通道：

```json
{"type": "subscribe", "channels": ["tasks", "cron"]}
```

```json
{"type": "subscribed", "channels": ["cron", "tasks"]}
```

取消订阅：

```json
{"type": "unsubscribe", "channels": ["cron"]}
```

目前 `tasks` 和 `cron` 已接入实时事件源。`sessions`、`memory`、`skills`、`channels`、`logs`、`analytics`、`knowledge` 是保留的订阅名，服务端暂未为它们接入事件源。未知通道返回 `subscribe_error`。

推送帧采用统一包装：

```json
{
  "type": "task_created",
  "payload": {
    "id": "task_123",
    "status": "completed"
  }
}
```

## 连接维护

- 服务端按 `gateway.ws_heartbeat_seconds` 配置发送 WebSocket 控制帧心跳，默认 30 秒。
- 客户端应处理连接关闭，并使用带抖动的指数退避重连。
- 重连后重新发送 `auth` 帧和原 `session_key` 可继续使用同一会话；服务端不提供消息回放协议。
- 反向代理的 WebSocket 空闲超时应大于心跳间隔。
