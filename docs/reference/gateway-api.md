# Gateway API 参考

**适用范围：独立 Python Runtime。** 桌面端操作见 [Desktop 使用指南](../desktop/index.md)。

Gateway 默认监听 `http://127.0.0.1:58123`，API 前缀为 `/api/v1`（可由 `gateway.api_prefix` 修改）。HTTP 接口由 `echo_agent/gateway/server.py` 和 `echo_agent/gateway/api/__init__.py` 注册；以下路径均为相对前缀。另有 `/meta`、`/playground`、`/ws` 和 `/ws/dashboard`，其中 WebSocket 路径可配置。

## 认证

默认令牌请求头为 `X-Echo-Agent-Token`，可用 `gateway.auth.token_header` 修改。`gateway.auth.mode` 控制用户身份的 open、allowlist 或 pairing 授权；API 令牌与用户配对是不同的检查。配置 `gateway.auth.api_tokens` 后，读取及消息接口要求有效令牌；管理接口使用 `gateway.auth.admin_tokens`，未单独配置时回退到 `api_tokens`。管理接口通过请求头读取令牌，不接受查询参数令牌。健康检查无需令牌。详情见[网关认证](../integrations/gateway/authentication.md)。

`POST /api/v1/pair` 需要 API 令牌，`POST /api/v1/pair/verify` 接受 `platform`、`user_id` 和 `code`；验证成功仅授权该平台与用户组合，不颁发新令牌。验证码默认有效 300 秒。

## 已注册端点

| 方法 | 路径（加在 `/api/v1` 后） | 用途 |
|---|---|---|
| `POST` | `/message` | 投递消息 |
| `GET` | `/health` | 健康状态 |
| `GET` | `/ready` | 本地任务执行就绪状态；不探测远程模型 |
| `GET` | `/stats` | 运行统计 |
| `GET` | `/capabilities` | 调用方能力 |
| `POST` | `/pair` | 生成配对码 |
| `POST` | `/pair/verify` | 验证配对码 |
| `GET` | `/sessions` | 列出会话 |
| `DELETE` | `/sessions/{key}` | 重置会话 |
| `GET` | `/sessions/{key}/history` | 会话历史 |
| `GET` | `/sessions/{key}/turns` | 会话回合状态 |
| `GET` | `/turns/{event_id}` | 按事件查询回合 |
| `GET` | `/memory` | 列出记忆 |
| `GET` | `/memory/stats` | 记忆统计 |
| `POST` | `/memory/search` | 检索记忆 |
| `GET` | `/memory/{id}` | 读取记忆 |
| `PUT` | `/memory/{id}` | 更新记忆 |
| `DELETE` | `/memory/{id}` | 删除记忆 |
| `GET` | `/skills` | 列出技能 |
| `POST` | `/skills/import` | 导入技能 |
| `POST` | `/skills/upload` | 上传技能 |
| `GET` | `/skills/{name}` | 技能详情 |
| `GET` | `/skills/{name}/deps` | 技能依赖状态 |
| `POST` | `/skills/{name}/deps/install` | 安装技能依赖 |
| `POST` | `/skills/{name}/toggle` | 切换技能状态 |
| `DELETE` | `/skills/{name}` | 删除技能 |
| `GET` | `/channels` | 列出通道 |
| `POST` | `/channels/{name}/{action}` | 通道生命周期操作 |
| `GET` | `/knowledge/status` | 知识库状态 |
| `POST` | `/knowledge/rebuild` | 重建索引 |
| `POST` | `/knowledge/upload` | 上传文档 |
| `GET` | `/knowledge/documents` | 列出文档 |
| `DELETE` | `/knowledge/documents/{path}` | 删除文档（允许多级路径） |
| `GET` | `/knowledge/jobs` | 列出索引任务 |
| `GET` | `/knowledge/jobs/{id}` | 索引任务详情 |
| `DELETE` | `/knowledge/jobs/{id}` | 取消索引任务 |
| `GET` | `/config` | 读取脱敏配置 |
| `PATCH` | `/config` | 更新配置 |
| `GET` | `/tasks` | 列出任务 |
| `POST` | `/tasks` | 创建任务 |
| `GET` | `/tasks/{id}` | 任务详情 |
| `PUT` | `/tasks/{id}` | 更新任务 |
| `DELETE` | `/tasks/{id}` | 删除任务 |
| `POST` | `/tasks/{id}/transition` | 切换任务状态 |
| `POST` | `/tasks/{id}/retry` | 重试任务 |
| `GET` | `/cron` | 列出定时任务 |
| `POST` | `/cron` | 创建定时任务 |
| `PUT` | `/cron/{id}` | 更新定时任务 |
| `DELETE` | `/cron/{id}` | 删除定时任务 |
| `POST` | `/cron/{id}/trigger` | 立即触发 |
| `GET` | `/cron/{id}/runs` | 运行历史 |
| `GET` | `/logs` | 查询日志 |
| `GET` | `/analytics/tokens` | Token 用量 |
| `GET` | `/analytics/skills` | 技能用量 |
| `GET` | `/analytics/channels` | 通道用量 |

`GET /sessions` 支持 `channel`、`q` 过滤。提供 `limit` 或 `offset` 时返回偏移分页；`limit` 为 1–500，默认 100。`GET /sessions/{key}/history` 支持 `limit`（1–500）和 `offset`，`total` 为完整可见历史条数，`returned` 为本页条数。`GET /sessions/{key}/turns` 的 `limit` 为 1–100，默认 20。其他列表接口的参数和响应结构以相应处理器为准，不存在统一的 `data/meta` 包装或游标分页协议。

`POST /message` 的请求体可包含 `platform`、`user_id`、`chat_id`、`text`；例如：

```bash
curl -X POST http://127.0.0.1:58123/api/v1/message \
  -H 'Content-Type: application/json' \
  -H 'X-Echo-Agent-Token: YOUR_TOKEN' \
  -d '{"platform":"api","user_id":"u1","chat_id":"c1","text":"生成周报"}'
```

错误响应通常为 `{"error": "..."}`，实际字段和状态码由具体端点决定。`GET {api_prefix}/health`（默认 `/api/v1/health`）根据健康状态返回 200 或 503。

`GET {api_prefix}/ready` 无需令牌，只有 Gateway、消息总线、Agent、SQLite 连接、非 Stub 模型配置及必需 Runtime 依赖均已就绪时返回 200；否则为 503，`checks` 给出各项结果。`provider_remote_verified: false` 表示未进行远程调用探活；200 不保证模型凭据、配额或上游网络可用。没有模型时可继续打开配置界面，`/health` 的存活状态不代表能完成真实任务。

`GET /config` 返回脱敏后的保存配置，`_meta.running` 是运行值，`_meta.pending_paths` 与 `_meta.restart_required` 描述两者差异。`PATCH /config` 仅原子保存经过校验的非敏感叶字段，不原地更改 Runtime；嵌套对象按叶合并并递归校验。通道的显式 start/stop/restart 会加载该通道的保存配置。配置加载仍遵循环境变量优先级。

`POST /cron/{id}/trigger` 对不存在任务返回 404；暂停、禁用、终态、已有执行、并发容量已满或执行锁冲突返回可诊断的 409，避免把尚在执行误报成任务不存在。

## 幂等重试 {#idempotent-retries}

投递消息的接口支持幂等键，用于在网络抖动、超时重连后安全重试，而不会让同一条消息被处理两次。

### 携带幂等键

| 入口 | 传递方式 |
|------|---------|
| `POST {api_prefix}/message` | `Idempotency-Key` 或 `X-Idempotency-Key` 请求头 |
| Webhook 通道 | 同上两个请求头，或请求体的 `idempotency_key` 字段 |
| WebSocket `message` 帧 | 帧内的 `idempotency_key` 字段 |

键的约束：非空字符串，最长 **200** 字符，不含控制字符。同时提供请求头和请求体且两者不一致时返回 400。

```bash
curl -X POST http://127.0.0.1:58123/api/v1/message \
  -H "Content-Type: application/json" \
  -H "Idempotency-Key: order-2026-0829-001" \
  -d '{"platform":"api","user_id":"u1","chat_id":"c1","text":"生成周报"}'
```

### 重试语义

相同键 + 相同请求内容 → 复用首次的事件与响应，**不会重复投递**：

```json
{"status": "accepted", "event_id": "38919935...", "session_key": "gateway:api:c1"}
```

相同键 + **不同**请求内容 → 409，请求被拒绝，不产生新事件：

```json
{"error": "idempotency key was already used for a different request"}
```

!!! warning "409 只表示键冲突"
    409 专用于「同一个键被用于不同内容」这一种情况，客户端**不应重试**——应换新键或修回原内容。
    任务未完成（`incomplete` / `interrupted`）可返回 200，须读取响应体的 `status`。相同幂等键只重放已保存终态，不创建新回合。若要继续执行，应先检查既有结果和外部副作用，再明确提交新请求；此机制不承诺任意外部副作用 exactly-once。

### 作用域与有效期

键的作用域包含调用方身份，不同 token 之间互不干扰：

| 入口 | 作用域组成 |
|------|-----------|
| HTTP | token 派生的 principal + `session_key` |
| WebSocket | 同上（按连接握手身份） |
| Webhook | `sender_id` + `chat_id` |

| 参数 | 值 |
|------|-----|
| 记录有效期 | 3600 秒（1 小时） |
| 进程内缓存条数 | Gateway 4096 / Webhook 2048 |
| 持久化记录上限 | 100000 条 |

记录同时写入 SQLite，因此**跨进程重启的重试仍可去重并重放结果**，不受单会话回合裁剪影响。存储不可用、或未过期记录已达容量上限时，接口 **fail closed** 返回 503 而不是放行可能重复执行的请求。

### 与 `wait` 的配合

`wait=false`（默认）时缓存的是投递回执（`accepted`）；`wait=true` 时缓存的是回合最终结果。若首个请求仍在处理中，并发的重试会等待同一结果而非重复触发，超时返回 504。

---



## 分页

分页参数由端点分别定义。`GET /sessions` 和 `GET /sessions/{key}/history` 支持 `limit` 与 `offset`；`GET /memory` 和 `GET /logs` 也支持偏移分页。`GET /sessions/{key}/turns` 仅支持 `limit`。不要向这些端点传入 `cursor`。
