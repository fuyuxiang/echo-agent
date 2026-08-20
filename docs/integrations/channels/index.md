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

- **BaseChannel** — 所有通道的抽象基类（`echo_agent/channels/base.py`），三个抽象方法必须实现：`start()`、`stop()`、`send()`；能力通过类属性声明，而非方法调用
- **MessageBus** — 消息总线，负责在通道与 Agent 核心之间传递标准化事件
- **ChannelManager** — 通道管理器，根据配置加载 `_CHANNEL_REGISTRY` 中的通道实例，处理启停与生命周期

### 能力声明

通道不实现某能力时无需覆写方法，只需保持对应类属性为默认的 `False`。Agent 核心据此决定投递方式：

```python
class BaseChannel(ABC):
    supports_edit: bool = False                  # 是否支持编辑已发送消息
    supports_reactions: bool = False              # 是否支持表情回应
    supports_files: bool = False                  # 是否能发送文件
    supports_interactive_choices: bool = False    # 是否支持交互式选项

    @abstractmethod
    async def start(self) -> None: ...

    @abstractmethod
    async def stop(self) -> None: ...

    @abstractmethod
    async def send(self, event: OutboundEvent) -> SendResult | None: ...
```

`send()` 返回 `SendResult | None` —— 全局 handler 收到不属于自己的事件时必须返回 `None`，否则会产生错误的投递回执。

## 通道能力矩阵

共 14 个通道，与 `echo_agent/channels/manager.py` 的 `_CHANNEL_REGISTRY` 一一对应。下表的能力列取自各适配器的类属性声明，未声明即为 `False`：

| 通道 | 注册名 | 连接方式 | 消息编辑 | 表情回应 | 发送文件 | 实时 |
|------|--------|----------|----------|----------|----------|------|
| Telegram | `telegram` | Long Polling | ✅ | ✅ | ❌ | ✅ |
| Discord | `discord` | WebSocket (Gateway) | ✅ | ✅ | ❌ | ✅ |
| Slack | `slack` | Socket Mode (WebSocket) | ✅ | ✅ | ❌ | ✅ |
| Matrix | `matrix` | Client-Server API (长轮询 sync) | ❌ | ✅ | ❌ | ✅ |
| 微信 | `weixin` | 长轮询 | ❌ | ❌ | ✅ | ✅ |
| QQ 机器人 | `qqbot` | WebSocket (Gateway) | ❌ | ❌ | ⚠️ | ✅ |
| 企业微信 | `wecom` | Webhook 回调 | ❌ | ❌ | ❌ | ✅ |
| 钉钉 | `dingtalk` | WebSocket | ❌ | ❌ | ❌ | ✅ |
| 飞书 / Lark | `feishu` | Webhook 事件订阅 | ❌ | ❌ | ❌ | ✅ |
| WhatsApp | `whatsapp` | Webhook 回调 | ❌ | ❌ | ❌ | ✅ |
| Email | `email` | IMAP 轮询 / SMTP 发送 | ❌ | ❌ | ❌ | ❌ |
| Webhook | `webhook` | HTTP POST | ❌ | ❌ | ❌ | ❌ |
| CLI | `cli` | stdin/stdout | ❌ | ❌ | ❌ | ✅ |
| 定时任务 | `cron` | 内部调度触发 | ❌ | ❌ | ❌ | ❌ |

「实时」对应类属性 `is_realtime`。`email`、`webhook`、`cron` 为异步通道，其余为实时通道。所有通道的 `supports_interactive_choices` 目前均为 `False`。

> ⚠️ QQ 机器人的文件能力在运行时按配置决定：`supports_files` 由 `config.media_enabled` 赋值，未开启媒体时为 `False`。

发送文件前请通过 `supports_files` 判断，不要假设通道都能发文件 —— 目前只有 `weixin` 恒定为 `True`，`qqbot` 视配置而定。

## 各通道简介

### 海外 IM

- **Telegram** — 能力最完整的通道，支持消息编辑与表情回应。发送时对 `<`、`>`、`&` 做 HTML 转义；确实需要发送原始标记时用 `metadata["telegram_markup"] = True` 显式退出转义。
- **Discord** — 通过 Bot Gateway (WebSocket) 接入，支持消息编辑与表情回应。
- **Slack** — Socket Mode 接入，无需公网 IP，支持消息编辑与表情回应。
- **Matrix** — 开放协议，通过 Client-Server API 的 sync 长轮询接收，支持表情回应。
- **WhatsApp** — 通过 Webhook 回调接收消息。

### 国内 IM

- **微信 (`weixin`)** — 长轮询接入，是当前唯一恒定支持发送文件的通道。
- **QQ 机器人 (`qqbot`)** — 通过 Gateway (WebSocket) 接入；开启 `media_enabled` 后支持发送文件。
- **企业微信 (`wecom`)** — 企业内部应用，通过回调接收消息，消息体加解密见 `wecom_crypto.py`。
- **钉钉 (`dingtalk`)** — 通过 WebSocket 长连接接入，无需公网回调地址。
- **飞书 / Lark (`feishu`)** — 通过 Webhook 事件订阅接入。

### 非 IM 通道

- **CLI (`cli`)** — 命令行交互通道，零配置即可使用。
- **Email (`email`)** — IMAP 轮询收信、SMTP 发信。
- **Webhook (`webhook`)** — 以 HTTP POST 接入外部系统，适合 CI/CD 触发、告警接入等场景。
- **定时任务 (`cron`)** — 由内部调度按计划触发，没有外部对话方，用于周期性主动任务。

## 通用配置

大多数通道共享以下基础配置项（完整逐项说明见[配置参考](../../reference/configuration.md)，该页由 schema 自动生成）：

```yaml
channels:
  telegram:
    enabled: true                    # 是否启用该通道
    token: "..."                     # 平台凭据，字段名随通道而异
    allow_from:                      # 发送者白名单（空 = 不限制）
      - "user_id_1"
    group_policy: "mention"          # 群聊策略: open | mention
```

各通道的专有字段（如 Telegram 的 `proxy`、`data_dir`、`reactions_enabled`）请查阅对应的通道页面。

### allow_from 白名单

控制哪些用户可以与 Agent 交互，判定逻辑在 `BaseChannel.is_allowed()`：

- 留空或不配置 — 接受所有用户消息
- 配置用户 ID 列表 — 只响应列表内的用户，其余静默丢弃

### group_policy 群聊策略

控制 Agent 在群聊中的触发条件，取值只有两个：

| 策略 | 行为 |
|------|------|
| `mention` | 仅在被 @提及 时回复（默认） |
| `open` | 响应群内所有消息 |

不想处理群聊时，用白名单限制发送者，或直接关闭该通道。

## 流式输出与渐进式投递

LLM 响应是流式生成的。是否对某通道启用流式增量回复由 `stream_channels` 决定，默认包含 `cli`、`telegram`、`discord`、`slack` 和 `gateway:*`（支持 `prefix:*` 通配）。启用后，支持编辑的通道会不断更新已发出的消息，形成"打字机效果"；不支持编辑的通道则等生成完毕一次性发送。

### 推送节流

每次增量更新对 IM 通道都意味着一次编辑 API 调用，因此推送受字符阈值与时间间隔双重控制，满足任一条件即推送一段：

| 配置项 | 默认值 | 作用 |
|--------|--------|------|
| `stream_flush_chars` | 180 | 累计字符数达到阈值即推送 |
| `stream_flush_interval_ms` | 1500 | 两次推送之间的最大间隔（毫秒） |
| `stream_paragraph_mode` | `true` | 优先在段落边界切分 |

本地通道（`cli`、`gateway` websocket）没有 API 限频压力，帧成本极低，因此单独走一档低延迟参数：

| 配置项 | 默认值 | 作用 |
|--------|--------|------|
| `stream_local_flush_chars` | 24 | 本地通道字符阈值，设为 `0` 则复用通用配置 |
| `stream_local_flush_interval_ms` | 100 | 本地通道最大推送间隔（毫秒） |

调大通用档的两个值可以减少编辑调用、降低触发平台限频的概率，代价是回复看起来更"卡顿"。
