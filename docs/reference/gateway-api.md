# Gateway API 参考

Echo Agent Gateway 提供 HTTP REST API 和 WebSocket 端点，用于外部系统集成和 Dashboard 通信。

## 基本信息

| 项目 | 值 |
|------|------|
| 默认地址 | `http://127.0.0.1:8080` |
| 协议 | HTTP/1.1, WebSocket |
| 内容类型 | `application/json` |
| 认证方式 | Token Header / Pairing |

---

## 认证

### 认证模式

| 模式 | 说明 | 配置值 |
|------|------|--------|
| open | 无认证（仅本地使用） | `gateway.auth.mode: open` |
| allowlist | Token 白名单 | `gateway.auth.mode: allowlist` |
| pairing | 配对码认证 | `gateway.auth.mode: pairing` |

### Token 认证

请求时在 Header 中携带 Token：

```http
GET /api/sessions HTTP/1.1
Host: localhost:8080
X-Echo-Agent-Token: your-api-token-here
```

Token Header 名称可通过 `gateway.auth.token_header` 自定义。

### 权限级别

| Token 类型 | 权限 |
|-----------|------|
| `api_tokens` | 读取 + 会话操作 |
| `admin_tokens` | 全部操作（含配置修改、关停） |

### 安全防护

- **Origin 检查**: 仅允许 `gateway.auth.allowed_origins` 中列出的来源
- **Host 检查**: 仅允许 `gateway.auth.allowed_hosts` 中列出的 Host
- **Pairing 码过期**: 默认 300 秒（`gateway.auth.pairing_ttl_seconds`）

---

## REST API 端点

### 会话管理

#### GET /api/sessions

列出所有会话。

**请求参数（Query）**:

| 参数 | 类型 | 说明 |
|------|------|------|
| `status` | string | 过滤状态：active / idle / closed |
| `limit` | int | 返回数量限制 |
| `offset` | int | 分页偏移 |

**响应示例**:

```json
{
  "sessions": [
    {
      "id": "sess_abc123",
      "status": "active",
      "channel": "slack",
      "created_at": "2024-01-15T10:30:00Z",
      "last_message_at": "2024-01-15T11:45:00Z",
      "message_count": 42
    }
  ],
  "total": 15,
  "limit": 10,
  "offset": 0
}
```

#### POST /api/sessions

创建新会话。

**请求体**:

```json
{
  "channel": "api",
  "metadata": {
    "user": "admin"
  }
}
```

**响应**: 201 Created

```json
{
  "id": "sess_new456",
  "status": "active",
  "channel": "api",
  "created_at": "2024-01-15T12:00:00Z"
}
```

---

### 记忆管理

#### GET /api/memory

查询记忆条目。

| 参数 | 类型 | 说明 |
|------|------|------|
| `query` | string | 搜索关键词 |
| `key` | string | 精确键名 |
| `limit` | int | 返回数量 |

#### POST /api/memory

写入记忆。

```json
{
  "key": "user_preference",
  "value": "偏好暗色主题",
  "metadata": {
    "source": "api",
    "ttl": 86400
  }
}
```

#### DELETE /api/memory/{key}

删除指定记忆条目。需要 admin 权限。

---

### 知识库

#### GET /api/knowledge

查询知识库。

| 参数 | 类型 | 说明 |
|------|------|------|
| `query` | string | 语义搜索查询 |
| `top_k` | int | 返回结果数 |
| `filter` | string | 元数据过滤（JSON） |

#### POST /api/knowledge

添加知识条目。

```json
{
  "content": "Echo Agent 支持多通道集成...",
  "metadata": {
    "source": "docs",
    "category": "features"
  }
}
```

#### DELETE /api/knowledge/{id}

删除知识条目。需要 admin 权限。

---

### 技能管理

#### GET /api/skills

列出所有技能。

| 参数 | 类型 | 说明 |
|------|------|------|
| `status` | string | active / staged / disabled |
| `category` | string | 按分类过滤 |

#### GET /api/skills/{id}

获取技能详情。

#### POST /api/skills/{id}/enable

启用技能。

#### POST /api/skills/{id}/disable

禁用技能。

---

### 任务管理

#### GET /api/tasks

列出任务。

| 参数 | 类型 | 说明 |
|------|------|------|
| `status` | string | pending / running / completed / failed |
| `session_id` | string | 按会话过滤 |

#### POST /api/tasks

创建任务。需要 admin 权限。成功返回 `201` 与 `{"task": {...}}`。

```json
{
  "title": "整理本周会议纪要",
  "description": "按项目分组，输出到知识库",
  "priority": 5,
  "labels": ["weekly"],
  "assignee": "",
  "source": "human",
  "board_id": "default",
  "parent_task_id": "",
  "metadata": {}
}
```

只有 `title` 必填，为空返回 `400`。`priority` 必须是整数（默认 `5`，布尔值不接受），`labels` 必须是字符串数组，`metadata` 必须是对象，否则均返回 `400`。`parent_task_id` 用于挂到父任务下。

同样的字段校验也适用于 `PATCH /api/tasks/{id}`，其中只校验请求里实际出现的字段。

---

### 定时任务

#### GET /api/cron

列出定时任务。

#### POST /api/cron

创建定时任务。

```json
{
  "schedule": "0 9 * * 1-5",
  "task": "发送每日工作摘要",
  "enabled": true
}
```

#### PUT /api/cron/{id}

更新定时任务。

#### DELETE /api/cron/{id}

删除定时任务。需要 admin 权限。

---

### 通道管理

#### GET /api/channels

列出所有通道及其状态。

**响应示例**:

```json
{
  "channels": [
    {
      "name": "slack",
      "status": "connected",
      "connected_at": "2024-01-15T08:00:00Z",
      "message_count": 1234
    },
    {
      "name": "telegram",
      "status": "disconnected",
      "last_error": "Connection timeout"
    }
  ]
}
```

#### POST /api/channels/{name}/reconnect

触发通道重连。

---

### 分析统计

#### GET /api/analytics

获取使用统计数据。

| 参数 | 类型 | 说明 |
|------|------|------|
| `period` | string | today / week / month |
| `group_by` | string | model / tool / channel |

**响应示例**:

```json
{
  "period": "today",
  "total_messages": 156,
  "total_tokens": 234567,
  "total_cost_usd": 2.34,
  "by_model": {
    "claude-sonnet-4-20250514": {
      "requests": 89,
      "tokens": 156000,
      "cost_usd": 1.87
    }
  },
  "by_tool": {
    "search": 45,
    "memory": 23,
    "filesystem": 12
  }
}
```

---

### 配置

#### GET /api/config

获取当前配置（脱敏）。需要 admin 权限。

#### PATCH /api/config

运行时修改配置。需要 admin 权限。

```json
{
  "models.primary.temperature": 0.5,
  "tools.approval_mode": "auto"
}
```

!!! warning "运行时配置"
    通过 API 修改的配置仅在当前运行期间生效，不会持久化到配置文件。重启后恢复文件配置。

---

### 日志

#### GET /api/logs

获取系统日志。

| 参数 | 类型 | 说明 |
|------|------|------|
| `level` | string | 最低级别过滤 |
| `since` | string | 起始时间（ISO 8601） |
| `limit` | int | 返回行数，默认 100 |
| `follow` | bool | SSE 模式持续推送 |

---

### 生命周期

#### GET /api/lifecycle/health

健康检查端点。无需认证。

**响应**:

```json
{
  "status": "healthy",
  "version": "0.3.8",
  "uptime_seconds": 86400,
  "checks": {
    "database": "ok",
    "models": "ok",
    "channels": "degraded"
  }
}
```

| 状态 | HTTP 码 | 说明 |
|------|---------|------|
| healthy | 200 | 所有组件正常 |
| degraded | 200 | 部分组件异常但可服务 |
| unhealthy | 503 | 无法正常服务 |

#### POST /api/lifecycle/shutdown

优雅关停。需要 admin 权限。

```json
{
  "timeout": 30,
  "reason": "Maintenance"
}
```

---

## 通用响应格式

### 成功响应

```json
{
  "data": { ... },
  "meta": {
    "request_id": "req_abc123",
    "timestamp": "2024-01-15T12:00:00Z"
  }
}
```

### 错误响应

```json
{
  "error": {
    "code": "UNAUTHORIZED",
    "message": "Invalid or expired token",
    "details": {}
  },
  "meta": {
    "request_id": "req_abc123",
    "timestamp": "2024-01-15T12:00:00Z"
  }
}
```

### 错误码

| HTTP 状态码 | 错误码 | 说明 |
|------------|--------|------|
| 400 | BAD_REQUEST | 请求参数无效 |
| 401 | UNAUTHORIZED | 未认证或 Token 无效 |
| 403 | FORBIDDEN | 权限不足 |
| 404 | NOT_FOUND | 资源不存在 |
| 409 | CONFLICT | 资源冲突 |
| 429 | RATE_LIMITED | 请求频率超限 |
| 500 | INTERNAL_ERROR | 服务器内部错误 |
| 503 | UNAVAILABLE | 服务不可用 |

---

## 频率限制

API 请求受频率限制控制。限制信息通过响应头返回：

```http
X-RateLimit-Limit: 60
X-RateLimit-Remaining: 45
X-RateLimit-Reset: 1705312800
```

## 分页

分页统一为 offset-based，通过 `limit` 与 `offset` 查询参数控制，没有 cursor 分页。

各端点的默认 `limit` 并不统一，需按端点查阅：

| 端点 | 默认 `limit` | 支持 `offset` |
|------|-------------|--------------|
| `/api/logs` | 200 | 是 |
| `/api/memory` | 50 | 是 |
| `/api/sessions` | 100 | 否 |
| `/api/sessions/{key}/history` | 100 | 否 |
| cron 执行历史 | 10 | 否 |

未提供 `offset` 的端点只能取首屏，无法翻页。

`/api/sessions/{key}/history` 的 `limit` 取值范围是 `1`–`500`，越界返回 400。响应里
`total` 是整段可见历史的条数（用于判断是否还有更早的记录），`returned` 才是本次返回
的条数——两者不要混用。
