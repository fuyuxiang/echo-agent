# 消息通道

Echo Agent 的通道体系负责将来自不同平台的消息统一接入核心处理流程。每个通道实现为一个独立的适配器，继承自 `BaseChannel` 基类，通过 `ChannelManager` 统一管理生命周期。

## 核心架构

### 三层结构

```
用户消息 → Channel Adapter → MessageBus → Agent Core
                                ↑
                          ChannelManager
                        (注册/启停/路由)
```

- **BaseChannel** — 所有通道的抽象基类，定义统一接口：`connect()`、`disconnect()`、`send()`、`onMessage()`，以及能力声明方法 `getCapabilities()`
- **MessageBus** — 消息总线，负责在通道与 Agent 核心之间传递标准化消息对象（`IncomingMessage` / `OutgoingMessage`）
- **ChannelManager** — 通道管理器，根据配置文件动态加载通道实例，处理启停、健康检查与故障恢复

### 消息标准化

所有通道收到的原始消息都会被转换为统一的 `IncomingMessage` 格式：

```typescript
interface IncomingMessage {
  id: string;
  channel: string;        // 通道标识符
  sender: string;         // 发送者 ID
  content: string;        // 文本内容
  attachments?: File[];   // 附件列表
  replyTo?: string;       // 引用消息 ID
  metadata: Record<string, any>;  // 通道特有元数据
}
```

## 通道能力矩阵

各通道因平台限制具备不同能力。Agent 核心根据能力矩阵决定消息的投递方式（如：不支持编辑的通道只收到最终回复，不发送中间流式更新）。

| 通道 | 连接方式 | 消息编辑 | 表情回应 | 文件收发 | 实时 | 群聊 | 白名单 |
|------|----------|----------|----------|----------|------|------|--------|
| Telegram | Long Polling / Webhook | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Discord | WebSocket (Gateway) | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| WeChat Work | Webhook 回调 | ❌ | ❌ | ✅ | ✅ | ✅ | ✅ |
| DingTalk | Webhook 回调 | ❌ | ❌ | ✅ | ✅ | ✅ | ✅ |
| Feishu (Lark) | Event Subscription | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Slack | Socket Mode / Events API | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Matrix | Client-Server API (Sync) | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Email (IMAP/SMTP) | IMAP IDLE | ❌ | ❌ | ✅ | ⚠️ | ❌ | ✅ |
| Web Chat | WebSocket | ✅ | ✅ | ✅ | ✅ | ❌ | ❌ |
| CLI | stdin/stdout | ✅ (ANSI覆写) | ❌ | ❌ | ✅ | ❌ | ❌ |
| Webhook (入站) | HTTP POST | ❌ | ❌ | ✅ | ❌ | ❌ | ✅ |
| Webhook (出站) | HTTP POST 回调 | ❌ | ❌ | ✅ | ❌ | ❌ | ✅ |
| SMS (Twilio) | Webhook 回调 | ❌ | ❌ | ❌ | ⚠️ | ❌ | ✅ |
| Voice (Twilio) | WebSocket 流 | ❌ | ❌ | ❌ | ✅ | ❌ | ✅ |

> ⚠️ = 有延迟或受平台限制（Email 依赖 IDLE 推送间隔；SMS 受运营商投递延迟影响）

## 各通道简介

### IM 类

- **Telegram** — 功能最完整的通道之一。支持 Bot API 的 Long Polling 与 Webhook 两种模式，Markdown/HTML 格式消息，Inline Keyboard 交互。
- **Discord** — 通过 Discord Bot Gateway 接入，支持 Slash Commands、Thread 会话、Embed 富文本。
- **Slack** — 支持 Socket Mode（无需公网 IP）与 Events API，集成 Block Kit 交互组件。
- **Matrix** — 开放协议，支持端到端加密房间。适合自建基础设施的场景。
- **WeChat Work (企业微信)** — 企业内部通讯，通过应用回调接收消息，主动发送接口推送回复。
- **DingTalk (钉钉)** — 与企业微信类似，支持企业内部应用与群机器人两种模式。
- **Feishu (飞书/Lark)** — 字节系办公平台，支持事件订阅、消息卡片、富文本回复。

### 通用类

- **Web Chat** — 内置的网页聊天组件，通过 WebSocket 与 Gateway 直连，适合嵌入产品页面。
- **CLI** — 命令行交互通道，零配置即可使用。流式输出通过 ANSI 转义序列实现逐字覆写。
- **Email (IMAP/SMTP)** — 邮件通道，使用 IMAP IDLE 监听新邮件，SMTP 发送回复。支持附件。

### Webhook 类

- **Webhook (入站)** — 接收外部系统的 HTTP POST 请求作为消息输入，适合 CI/CD 触发、告警接入等。
- **Webhook (出站)** — Agent 处理完成后，将结果 POST 到配置的外部 URL，适合异步通知。

### 通信类

- **SMS (Twilio)** — 通过 Twilio API 收发短信，适合通知、验证码等短消息场景。
- **Voice (Twilio)** — 通过 Twilio Voice WebSocket 实现语音对话，支持 STT/TTS 流式处理。

## 通用配置

所有通道共享一组基础配置项：

```yaml
channels:
  <channel_name>:
    enabled: true                    # 是否启用
    allow_from:                      # 发送者白名单（空 = 允许所有）
      - "user_id_1"
      - "user_id_2"
    group_policy: "mention_only"     # 群聊策略: mention_only | all | disabled
    max_message_length: 4096         # 单条消息最大长度
    timeout: 30000                   # 请求超时(ms)
    retry:
      max_attempts: 3               # 最大重试次数
      backoff: "exponential"        # 重试策略
```

### allow_from 白名单

白名单控制哪些用户可以与 Agent 交互：

- 留空或不配置 — 接受所有用户消息
- 配置用户 ID 列表 — 只响应列表内的用户，其他消息静默丢弃
- 支持通配符 — 如 `"group:*"` 允许所有群聊，`"admin:*"` 允许所有管理员

### group_policy 群聊策略

控制 Agent 在群聊中的触发条件：

| 策略 | 行为 |
|------|------|
| `mention_only` | 仅在被 @提及 时回复（默认） |
| `all` | 响应群内所有消息 |
| `disabled` | 不处理群聊消息 |

## 流式输出与渐进式投递

Echo Agent 的 LLM 响应是流式生成的。对于支持消息编辑的通道，系统会实时更新已发送的消息，实现"打字机效果"：

```
流式 Token → 是否支持编辑？
              ├─ 是 → 每 N 个 token 调用 editMessage() 更新内容
              └─ 否 → 等待生成完毕，一次性发送最终结果
```

### 编辑节流

为避免 API 限频，编辑操作受节流控制：

| 通道 | 最小编辑间隔 | 说明 |
|------|-------------|------|
| Telegram | 1000ms | Bot API 全局限 30 msg/s |
| Discord | 500ms | Rate limit per channel |
| Slack | 1000ms | Web API tier 限制 |
| Feishu | 500ms | 开放平台限频 |
| Web Chat | 100ms | 本地 WebSocket，无外部限制 |
| CLI | 50ms | 终端刷新率 |

### 降级策略

当通道暂时不可用（网络中断、API 限频）时，系统按以下策略处理：

1. **重试** — 按配置的重试策略自动重试
2. **缓冲** — 消息进入本地队列，恢复后按序发送
3. **降级通知** — 超过缓冲阈值后，通过备选通道通知用户
4. **丢弃** — 达到最大缓冲时间后丢弃过期消息（可配置）
