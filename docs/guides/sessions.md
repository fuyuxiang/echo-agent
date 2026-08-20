# 会话管理

Echo Agent 的会话系统为每个对话提供隔离的上下文环境，支持多通道、多用户的并发交互。

## 会话键组成

会话键由通道与会话两段组成，形如 `{channel}:{chat_id}`，实现见 `InboundEvent.session_key`：

| 组成部分 | 来源 | 说明 |
|----------|------|------|
| `channel` | `event.channel` | 通道注册名，如 `slack`、`weixin`、`telegram` |
| `chat_id` | `event.chat_id` | 平台侧的会话标识，私聊与群聊都用这一字段 |

```text
slack:C67890
```

若事件带有 `session_key_override`，则直接采用该值，不再按上述规则拼接。

### 群聊内按人隔离

群聊场景下可以进一步把发送者纳入键，由 `scoped_session_key(scope)` 决定：

| scope | 私聊 | 群聊 |
|-------|------|------|
| `shared` | `slack:C67890` | `slack:C67890`（整群共用一个会话） |
| `per_user` | `slack:D1` | `slack:C67890:U12345`（群内每人独立） |

`per_user` 只在群聊且存在 `sender_id` 时才追加发送者后缀；私聊下两种策略结果一致。拼接是幂等的，已含发送者后缀的键不会被重复拼接。

!!! note "隔离模型"
    同一用户在不同通道、不同群组中的消息互不干扰。是否在群内按人隔离取决于 `scope` 策略，而非会话键本身的结构。

## 会话生命周期

```
创建 → 活跃(active) → 过期(expired) → 归档(archived)
```

### 创建

当收到新的复合键对应的首条消息时，系统自动创建会话。会话数据类包含以下字段：

- `key` — 复合键
- `messages` — 消息列表
- `created_at` — 创建时间
- `updated_at` — 最后更新时间
- `metadata` — 元数据
- `last_consolidated` — 最后合并位置
- `status` — 状态（active / expired / archived）

### 活跃

会话处于活跃状态时，持续接收和处理消息。每次交互更新 `updated_at` 时间戳。

### 过期

当会话超过配置的 `expiry_hours` 未活动时，状态变为 `expired`。过期会话不再接受新消息。

!!! question "需维护者确认"
    过期会话是否在特定条件下可被重新激活？还是必须创建新会话？

### 归档

过期会话经过归档流程后状态变为 `archived`。

!!! question "需维护者确认"
    归档会话的存储位置和保留策略是什么？归档操作是定时批量执行还是逐个触发？

## 历史消息管理

### 消息数量限制

`get_history(max_messages)` 方法返回最近的消息记录，默认上限 500 条。

### 合并边界对齐

历史消息截取时自动对齐到安全边界，确保不会产生孤立的 `tool-result` 消息。

!!! warning "合并边界"
    如果截取点落在 tool-call / tool-result 消息对中间，系统会自动向前调整，
    避免返回没有对应 tool-call 的孤立 tool-result 消息。

### 添加消息

```python
session.add_message(role="user", content="你好")
session.add_message(role="assistant", content="你好！有什么可以帮助你的？")
```

## 配置

通过 `SessionConfig` 配置会话行为：

```yaml
session:
  max_history_messages: 500      # 历史消息最大条数
  expiry_hours: 72               # 会话过期时间（小时）
  context_window_tokens: 128000  # 上下文窗口 token 数
```

| 参数 | 默认值 | 说明 |
|------|--------|------|
| `max_history_messages` | 500 | 单次获取历史的最大消息数 |
| `expiry_hours` | 72 | 无活动后会话过期时间 |
| `context_window_tokens` | — | 上下文窗口 token 限制 |

## 多通道会话隔离

Session Manager 确保不同通道间的会话完全隔离：

```yaml
# 同一用户在不同通道的独立会话
- channel: slack
  user: alice
  chat: general
  thread: null

- channel: wechat
  user: alice
  chat: group-01
  thread: null
```

!!! info "跨通道搜索"
    `session_search` 工具支持跨通道搜索会话历史，但会话上下文本身保持隔离。

## WebSocket 会话

`ws_session.py` 提供 WebSocket 长连接的会话管理能力：

- 维护 WebSocket 连接与会话的映射关系
- 处理连接断开后的会话状态保持
- 支持会话在连接恢复后续接

会话的空闲回收由网关的会话策略控制，单位是分钟：

```yaml
gateway:
  session_policy:
    idle_timeout_minutes: 1440   # 会话空闲超时（分钟），默认 1440 即 24 小时
```

## 会话搜索

`session_search` 工具用于在会话历史中检索信息：

- 支持跨会话的全文搜索
- 可按通道、用户、时间范围过滤
- 返回匹配消息及其上下文

## Dashboard 会话页面

Dashboard 的 Sessions 页面提供会话的可视化管理：

- 查看所有活跃会话列表
- 查看会话详情和消息历史
- 按通道、用户筛选会话
- 监控会话状态和过期情况

## 相关文件

| 文件 | 职责 |
|------|------|
| `session/manager.py` | 会话管理器：隔离、持久化、过期、归档 |
| `gateway/session_context.py` | 网关层会话上下文 |
| `gateway/session_policy.py` | 会话策略配置 |
| `gateway/ws_session.py` | WebSocket 会话管理 |
