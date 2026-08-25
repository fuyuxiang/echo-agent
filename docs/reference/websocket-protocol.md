# WebSocket 协议参考

Echo Agent Gateway 提供两个 WebSocket 端点，用于实时双向通信。

## 端点概览

| 端点 | 路径 | 用途 |
|------|------|------|
| 会话通信 | `/ws/session` | 与 Agent 实时交互 |
| Dashboard 推送 | `/ws/dashboard` | 接收系统状态实时更新 |

---

## 连接建立

### 认证

WebSocket 连接通过 URL 参数或首帧消息进行认证：

**方式一：URL 参数**

```
ws://localhost:8080/ws/session?token=your-api-token
```

**方式二：首帧认证**

```json
{
  "type": "auth",
  "token": "your-api-token"
}
```

!!! warning "认证时限"
    使用首帧认证时，连接建立后必须在 5 秒内发送 auth 帧，否则连接将被服务端关闭。

**方式三：请求头**

```http
Authorization: Bearer your-api-token
```

也可使用配置的 `token_header`（默认 `X-API-Token`）。

### 令牌来源与作用域

三种来源都能完成握手并取得 api 作用域，但**只有请求头与 auth 帧能取得 admin 作用域**：

| 来源 | 握手 | 只读帧 | 状态修改帧 |
|------|------|--------|------------|
| 请求头 | ✅ | ✅ | ✅ |
| auth 帧 | ✅ | ✅ | ✅ |
| URL `?token=` | ✅ | ✅ | ❌ |

`?token=` 会被 aiohttp 默认访问日志连同 query string 记下，也会进反向代理日志、
浏览器 history 与 referrer —— 令牌在日志里的存活期远长于其本身。因此任何改变状态的
帧（如 `skill.enable`、`skill.disable`）都不接受 URL 来源的令牌，与 HTTP 侧管理端点
同一口径。

这条规则**不依赖是否配置了 `admin_tokens`**。只配 `api_tokens` 的单令牌部署里，
api 令牌按回落规则充当 admin，同样受此限制。

!!! warning "URL 认证无法执行写操作"
    以 `?token=` 连接的客户端调用状态修改帧会收到 `admin token required` 错误。
    改用请求头或 auth 帧携带令牌即可。只读帧不受影响。

### 连接参数

| 参数 | 类型 | 说明 |
|------|------|------|
| `token` | string | API Token（仅 api 作用域，不能用于状态修改帧） |
| `session_id` | string | 恢复已有会话（可选） |
| `channel` | string | 通道标识，默认 `websocket` |

---

## /ws/session 协议

### 帧格式

所有消息使用 JSON 文本帧，结构如下：

```json
{
  "type": "消息类型",
  "id": "消息唯一 ID",
  "timestamp": "ISO 8601 时间戳",
  "payload": { }
}
```

### 客户端 → 服务端消息类型

#### user_message

发送用户消息。

```json
{
  "type": "user_message",
  "id": "msg_001",
  "timestamp": "2024-01-15T12:00:00Z",
  "payload": {
    "content": "帮我查询今天的天气",
    "attachments": []
  }
}
```

#### command

执行斜杠命令。

```json
{
  "type": "command",
  "id": "cmd_001",
  "timestamp": "2024-01-15T12:00:01Z",
  "payload": {
    "name": "approve",
    "args": ["req_abc123"]
  }
}
```

#### approval_response

响应工具审批请求。

```json
{
  "type": "approval_response",
  "id": "apr_001",
  "timestamp": "2024-01-15T12:00:02Z",
  "payload": {
    "request_id": "tool_req_xyz",
    "decision": "approve",
    "reason": null
  }
}
```

| decision 值 | 说明 |
|-------------|------|
| `approve` | 批准执行 |
| `deny` | 拒绝执行 |

#### clarification

提供澄清回复。

```json
{
  "type": "clarification",
  "id": "clr_001",
  "timestamp": "2024-01-15T12:00:03Z",
  "payload": {
    "content": "我指的是北京的天气",
    "request_id": "clarify_001"
  }
}
```

#### ping

心跳保活。

```json
{
  "type": "ping",
  "id": "ping_001",
  "timestamp": "2024-01-15T12:00:04Z",
  "payload": {}
}
```

---

### 服务端 → 客户端消息类型

#### agent_message

Agent 回复消息。

```json
{
  "type": "agent_message",
  "id": "resp_001",
  "timestamp": "2024-01-15T12:00:05Z",
  "payload": {
    "content": "北京今天晴，气温 25°C。",
    "format": "markdown",
    "metadata": {
      "model": "claude-sonnet-4-20250514",
      "tokens": {
        "input": 150,
        "output": 45
      },
      "latency_ms": 1200
    }
  }
}
```

#### agent_stream

流式回复片段（启用流式时）。

```json
{
  "type": "agent_stream",
  "id": "stream_001",
  "timestamp": "2024-01-15T12:00:05Z",
  "payload": {
    "message_id": "resp_001",
    "delta": "北京今天",
    "done": false
  }
}
```

最终片段：

```json
{
  "type": "agent_stream",
  "id": "stream_002",
  "timestamp": "2024-01-15T12:00:06Z",
  "payload": {
    "message_id": "resp_001",
    "delta": "",
    "done": true,
    "metadata": {
      "tokens": {"input": 150, "output": 45},
      "latency_ms": 1200
    }
  }
}
```

#### tool_call

Agent 正在调用工具（通知）。

```json
{
  "type": "tool_call",
  "id": "tc_001",
  "timestamp": "2024-01-15T12:00:06Z",
  "payload": {
    "tool": "search",
    "params": {
      "query": "北京天气"
    },
    "status": "executing"
  }
}
```

#### tool_result

工具执行结果。

```json
{
  "type": "tool_result",
  "id": "tr_001",
  "timestamp": "2024-01-15T12:00:07Z",
  "payload": {
    "tool": "search",
    "status": "success",
    "result_summary": "找到 3 条相关结果",
    "duration_ms": 450
  }
}
```

#### approval_request

请求用户审批工具调用。

```json
{
  "type": "approval_request",
  "id": "areq_001",
  "timestamp": "2024-01-15T12:00:08Z",
  "payload": {
    "request_id": "tool_req_xyz",
    "tool": "shell",
    "params": {
      "command": "npm install express"
    },
    "risk_level": "high",
    "description": "安装 npm 包 express"
  }
}
```

#### clarification_request

Agent 请求用户澄清。

```json
{
  "type": "clarification_request",
  "id": "creq_001",
  "timestamp": "2024-01-15T12:00:09Z",
  "payload": {
    "request_id": "clarify_001",
    "question": "你指的是哪个城市的天气？",
    "options": ["北京", "上海", "广州"]
  }
}
```

#### error

错误通知。

```json
{
  "type": "error",
  "id": "err_001",
  "timestamp": "2024-01-15T12:00:10Z",
  "payload": {
    "code": "TOOL_TIMEOUT",
    "message": "工具执行超时",
    "recoverable": true
  }
}
```

#### session_state

会话状态变更。

```json
{
  "type": "session_state",
  "id": "ss_001",
  "timestamp": "2024-01-15T12:00:11Z",
  "payload": {
    "status": "thinking",
    "detail": null
  }
}
```

| status 值 | 说明 |
|-----------|------|
| `idle` | 空闲等待输入 |
| `thinking` | 正在推理 |
| `tool_calling` | 正在调用工具 |
| `awaiting_approval` | 等待审批 |
| `awaiting_clarification` | 等待澄清 |

#### pong

心跳响应。

```json
{
  "type": "pong",
  "id": "pong_001",
  "timestamp": "2024-01-15T12:00:12Z",
  "payload": {}
}
```

---

## /ws/dashboard 协议

Dashboard WebSocket 仅接收服务端推送，客户端通常不发送业务消息。

### 事件类型

#### system_status

系统状态概览（定期推送）。

```json
{
  "type": "system_status",
  "timestamp": "2024-01-15T12:00:00Z",
  "payload": {
    "uptime_seconds": 86400,
    "active_sessions": 3,
    "memory_usage_mb": 256,
    "cpu_percent": 12.5,
    "channels": {
      "slack": "connected",
      "telegram": "connected"
    }
  }
}
```

#### session_event

会话事件通知。

```json
{
  "type": "session_event",
  "timestamp": "2024-01-15T12:00:01Z",
  "payload": {
    "event": "created",
    "session_id": "sess_abc123",
    "channel": "slack"
  }
}
```

| event 值 | 说明 |
|----------|------|
| `created` | 新会话创建 |
| `closed` | 会话关闭 |
| `message` | 新消息 |
| `tool_call` | 工具调用 |
| `error` | 会话错误 |

#### cost_update

费用实时更新。

```json
{
  "type": "cost_update",
  "timestamp": "2024-01-15T12:00:02Z",
  "payload": {
    "today_usd": 3.45,
    "month_usd": 67.89,
    "daily_limit": 10.0,
    "monthly_limit": 200.0
  }
}
```

#### skill_event

技能系统事件。

```json
{
  "type": "skill_event",
  "timestamp": "2024-01-15T12:00:03Z",
  "payload": {
    "event": "evolved",
    "skill_id": "skill_xyz",
    "skill_name": "code_review",
    "version": "1.2.0"
  }
}
```

#### log_entry

实时日志流。

```json
{
  "type": "log_entry",
  "timestamp": "2024-01-15T12:00:04Z",
  "payload": {
    "level": "WARNING",
    "logger": "echo_agent.tools.shell",
    "message": "Command execution timeout: 30s exceeded"
  }
}
```

---

## 连接管理

### 心跳机制

| 参数 | 值 |
|------|------|
| 心跳间隔 | 30 秒 |
| 超时判定 | 90 秒无 pong 响应 |
| 客户端行为 | 定期发送 `ping` 帧 |
| 服务端行为 | 收到 `ping` 立即回复 `pong` |

### 自动重连

TUI 客户端内置重连逻辑：

| 参数 | 值 |
|------|------|
| 初始重试延迟 | 1 秒 |
| 最大重试延迟 | 60 秒 |
| 退避策略 | 指数退避（2x） |
| 最大重试次数 | 无限（可配置） |
| 重连后行为 | 自动恢复会话（使用 session_id） |

### 关闭码

| 关闭码 | 含义 |
|--------|------|
| 1000 | 正常关闭 |
| 1001 | 服务端关停 |
| 1008 | 认证失败 |
| 1011 | 服务端内部错误 |
| 4000 | 会话不存在 |
| 4001 | Token 过期 |
| 4002 | 并发连接超限 |
| 4003 | 频率限制 |

---

## 客户端实现参考

### Python

```python
import asyncio
import websockets
import json

async def connect():
    uri = "ws://localhost:8080/ws/session?token=your-token"
    async with websockets.connect(uri) as ws:
        # 发送消息
        await ws.send(json.dumps({
            "type": "user_message",
            "id": "msg_001",
            "timestamp": "2024-01-15T12:00:00Z",
            "payload": {"content": "你好"}
        }))

        # 接收响应
        async for message in ws:
            data = json.loads(message)
            if data["type"] == "agent_message":
                print(data["payload"]["content"])
            elif data["type"] == "agent_stream":
                print(data["payload"]["delta"], end="", flush=True)
                if data["payload"]["done"]:
                    print()

asyncio.run(connect())
```

### JavaScript

```javascript
const ws = new WebSocket('ws://localhost:8080/ws/session?token=your-token');

ws.onopen = () => {
  ws.send(JSON.stringify({
    type: 'user_message',
    id: 'msg_001',
    timestamp: new Date().toISOString(),
    payload: { content: '你好' }
  }));
};

ws.onmessage = (event) => {
  const data = JSON.parse(event.data);
  switch (data.type) {
    case 'agent_message':
      console.log(data.payload.content);
      break;
    case 'agent_stream':
      process.stdout.write(data.payload.delta);
      if (data.payload.done) console.log();
      break;
    case 'approval_request':
      // 自动批准示例（生产环境应交由用户决策）
      ws.send(JSON.stringify({
        type: 'approval_response',
        id: `apr_${Date.now()}`,
        timestamp: new Date().toISOString(),
        payload: {
          request_id: data.payload.request_id,
          decision: 'approve'
        }
      }));
      break;
  }
};
```

!!! note "只处理文本帧"
    服务端仅处理 `TEXT` 类型的帧，帧内容按 JSON 解析。二进制帧不被处理，发送后不会有响应也不会报错——它们会被静默忽略，因此该协议不能用于文件传输。

    需要传输文件时走 HTTP 接口，或让 Agent 通过工具读取工作区内的文件路径。
