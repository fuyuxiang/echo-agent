# 新增 Channel 指南

本指南介绍如何为 Echo Agent 开发新的消息通道适配器，接入新的 IM 平台或通信协议。

## 架构概述

```
echo_agent/channels/
├── base.py              # BaseChannel 抽象基类
├── manager.py           # ChannelManager — 统一管理
├── telegram.py          # 参考实现：Telegram
├── webhook.py           # 参考实现：通用 Webhook
└── your_channel.py      # ← 你的新通道
```

消息流：

```
用户消息 → Channel.start() 监听
         → 构造 InboundEvent
         → bus.publish(InboundEvent)
         → AgentLoop 处理
         → OutboundEvent
         → Channel.send(OutboundEvent) → 用户
```

## BaseChannel 接口

```python
class BaseChannel(ABC):
    # 类属性声明
    name: str = "base"                        # 通道唯一标识
    supports_edit: bool = False               # 是否支持消息编辑
    supports_reactions: bool = False           # 是否支持表情回应
    is_realtime: bool = True                  # False 表示异步通道（email/cron）
    supports_interactive_choices: bool = False # 是否支持交互式选择
    supports_files: bool = False              # 是否支持文件上传

    def __init__(self, config: Any, bus: MessageBus):
        self.config = config
        self.bus = bus

    @abstractmethod
    async def start(self) -> None:
        """启动监听。"""

    @abstractmethod
    async def stop(self) -> None:
        """停止并清理资源。"""

    @abstractmethod
    async def send(self, event: OutboundEvent) -> SendResult | None:
        """发送消息到该通道。"""
```

## 步骤一：创建通道适配器

在 `echo_agent/channels/` 下新建文件，例如 `line.py`：

```python
"""LINE Messaging API channel adapter."""

from __future__ import annotations

import hmac
import hashlib
from typing import Any

import aiohttp
from loguru import logger

from echo_agent.bus.events import InboundEvent, OutboundEvent, ContentBlock, ContentType
from echo_agent.bus.queue import MessageBus
from echo_agent.channels.base import BaseChannel, SendResult


class LineChannel(BaseChannel):
    name = "line"
    supports_edit = False
    supports_reactions = True
    is_realtime = True
    supports_files = True

    def __init__(self, config: Any, bus: MessageBus):
        super().__init__(config, bus)
        self._token: str = config.line_channel_token or ""
        self._secret: str = config.line_channel_secret or ""
        self._webhook_path: str = "/webhook/line"
        self._session: aiohttp.ClientSession | None = None

    async def start(self) -> None:
        """注册 Webhook 路由，启动监听。"""
        self._session = aiohttp.ClientSession(
            headers={"Authorization": f"Bearer {self._token}"}
        )
        # 注册到 gateway 的 webhook 路由
        self._running = True
        logger.info("LINE channel started")

    async def stop(self) -> None:
        """关闭 HTTP 会话。"""
        self._running = False
        if self._session:
            await self._session.close()
            self._session = None

    async def handle_webhook(self, request: aiohttp.web.Request) -> aiohttp.web.Response:
        """处理 LINE Webhook 回调。"""
        body = await request.read()

        # 验证签名
        if not self._verify_signature(body, request.headers.get("X-Line-Signature", "")):
            return aiohttp.web.Response(status=403)

        data = await request.json()
        for event in data.get("events", []):
            if event["type"] == "message" and event["message"]["type"] == "text":
                inbound = InboundEvent(
                    channel=self.name,
                    chat_id=event["source"]["userId"],
                    user_id=event["source"]["userId"],
                    text=event["message"]["text"],
                    message_id=event["message"]["id"],
                    reply_token=event.get("replyToken", ""),
                )
                await self.bus.publish(inbound)

        return aiohttp.web.Response(status=200)

    async def send(self, event: OutboundEvent) -> SendResult | None:
        """通过 LINE Push API 发送消息。"""
        if not self.should_deliver(event):
            return SendResult(success=True, skipped=True)

        if not self._session:
            return SendResult(success=False, error="Session not initialized")

        payload = {
            "to": event.chat_id,
            "messages": [{"type": "text", "text": event.text}],
        }

        try:
            async with self._session.post(
                "https://api.line.me/v2/bot/message/push",
                json=payload,
            ) as resp:
                if resp.status == 200:
                    return SendResult(success=True)
                else:
                    body = await resp.text()
                    return SendResult(success=False, error=f"LINE API {resp.status}: {body}")
        except Exception as e:
            logger.error("LINE send error: {}", e)
            return SendResult(success=False, error=str(e))

    def _verify_signature(self, body: bytes, signature: str) -> bool:
        """验证 LINE Webhook 签名。"""
        digest = hmac.new(
            self._secret.encode(),
            body,
            hashlib.sha256,
        ).digest()
        import base64
        expected = base64.b64encode(digest).decode()
        return hmac.compare_digest(signature, expected)
```

## 步骤二：注册到 ChannelManager

注册表是 `echo_agent/channels/manager.py` 中的 `_CHANNEL_REGISTRY`：

```python
from echo_agent.channels.line import LineChannel

_CHANNEL_REGISTRY: dict[str, type[BaseChannel]] = {
    "cli": CLIChannel,
    "telegram": TelegramChannel,
    "discord": DiscordChannel,
    # ...
    "line": LineChannel,  # ← 新增
}
```

也可以在包外调用 `register_channel_type("line", LineChannel)` 注册，效果等同于直接写入字典。

!!! important "注册名必须与配置字段同名"
    `start_all()` 通过 `getattr(self.config, name)` 取出该通道的配置，其中 `self.config` 是 `ChannelsConfig`。因此注册名必须与 `ChannelsConfig` 上的字段名完全一致，否则取不到配置，通道会被静默跳过。当前 14 个注册名与配置字段是一一对应的。

## 步骤三：添加配置项

在 `echo_agent/config/schema.py` 中新增一个通道配置类，并挂到 `ChannelsConfig` 上。配置是嵌套结构，不是扁平的前缀字段：

```python
class LineChannelConfig(_Base):
    enabled: bool = Field(
        default=False,
        json_schema_extra={
            "status": "effective", "ref": "channels/manager.py:615",
            "desc_zh": "是否启用 LINE 通道",
            "desc_en": "Enable the LINE channel",
        },
    )
    channel_token: str = Field(default="", json_schema_extra={...})
    channel_secret: str = Field(default="", json_schema_extra={...})
    allow_from: list[str] = Field(default_factory=list, json_schema_extra={...})


class ChannelsConfig(_Base):
    # ...
    line: LineChannelConfig = Field(default_factory=LineChannelConfig)
```

对应的 YAML 形如（`line` 是本文假设的新通道，仓库中尚不存在）：

```yaml
channels:
  line:
    enabled: true
    channel_token: "..."
    channel_secret: "..."
    allow_from: []
```

每个字段的 `json_schema_extra` 会被 `echo_agent.config.docgen` 读取，用于生成[配置参考](../reference/configuration.md)。省略它不会影响运行，但该字段不会出现在生成的文档里。

## 步骤四：实现可选功能

可选方法在 `BaseChannel` 上均有默认实现，覆写时签名必须与基类一致。除 `send_typing`、`stop_typing`、`send_read_receipt` 返回 `None` 外，其余均返回 `SendResult`。

### 消息编辑

声明 `supports_edit = True` 后必须覆写本方法，流式回复依赖它更新已发出的消息。注意 `metadata` 与 `finalize` 是仅限关键字参数。

```python
async def edit_message(
    self,
    chat_id: str,
    message_id: str,
    text: str,
    *,
    metadata: dict[str, Any] | None = None,
    finalize: bool = False,
) -> SendResult:
    """编辑已发送的消息。finalize 表示这是本轮的最后一次编辑。"""
    ...
    return SendResult(success=True, message_id=message_id)
```

### 表情回应

声明 `supports_reactions = True` 后覆写这两个方法：

```python
async def send_reaction(
    self, chat_id: str, message_id: str, emoji: str,
    metadata: dict[str, Any] | None = None,
) -> SendResult:
    ...

async def remove_reaction(
    self, chat_id: str, message_id: str, emoji: str,
    metadata: dict[str, Any] | None = None,
) -> SendResult:
    ...
```

### 其他可选方法

| 方法 | 返回值 | 说明 |
|------|--------|------|
| `send_typing(chat_id, metadata=None)` | `None` | 显示"正在输入"状态 |
| `stop_typing(chat_id)` | `None` | 结束输入状态 |
| `send_read_receipt(chat_id, message_id, metadata=None)` | `None` | 已读回执 |
| `send_poll(chat_id, poll, metadata=None)` | `SendResult` | 发送投票，`poll` 为 `PollRequest` |
| `delete_message(chat_id, message_id, metadata=None)` | `SendResult` | 删除消息 |
| `send_voice(chat_id, audio_source, metadata=None)` | `SendResult` | 发送语音 |

### 文件发送

文件能力通过类属性声明，而非覆写方法：将 `supports_files` 设为 `True`，实际发送在 `send()` 中依据事件内容处理。上层的 `send_file` 工具会先检查该属性，为假时不会尝试投递。

## should_deliver 机制

`BaseChannel` 内置了消息过滤逻辑：

- `supports_edit=True` 的通道：接收所有消息（中间结果可被后续编辑覆盖）
- `supports_edit=False` 的通道：只接收 `is_final=True` 的消息 + heartbeat + approval_prompt

你无需重写此逻辑，除非有特殊需求。

## SendResult 规范

```python
@dataclass
class SendResult:
    success: bool               # 是否成功
    message_id: str = ""        # 平台返回的消息 ID（用于后续编辑）
    error: str = ""             # 错误信息
    skipped: bool = False       # 是否被 should_deliver 跳过
```

## 步骤五：编写测试

```python
"""tests/test_line_channel.py"""

import pytest
from unittest.mock import AsyncMock, MagicMock

from echo_agent.channels.line import LineChannel
from echo_agent.bus.events import OutboundEvent


@pytest.fixture
def channel():
    config = MagicMock()
    config.channel_token = "test-token"
    config.channel_secret = "test-secret"
    config.allow_from = []
    bus = MagicMock()
    return LineChannel(config, bus)


@pytest.mark.asyncio
async def test_send_success(channel):
    channel._session = MagicMock()
    mock_resp = MagicMock()
    mock_resp.status = 200
    channel._session.post = MagicMock(
        return_value=AsyncMock(__aenter__=AsyncMock(return_value=mock_resp))
    )

    event = OutboundEvent.text_reply(channel="line", chat_id="user123", text="Hello")
    result = await channel.send(event)
    assert result.success


@pytest.mark.asyncio
async def test_non_final_is_not_delivered(channel):
    event = OutboundEvent.text_reply(
        channel="line", chat_id="user123", text="thinking...", is_final=False
    )
    assert channel.should_deliver(event) is False
```

!!! warning "构造事件用 text_reply()，不要直接传 text="
    `OutboundEvent` 与 `InboundEvent` 的正文字段是 `content: list[ContentBlock]`，没有 `text` 构造参数 —— `OutboundEvent(text="hi")` 会抛 `TypeError`。发送方向用类方法 `OutboundEvent.text_reply(channel=..., chat_id=..., text=...)`；读取方向用 `event.text` 属性，它会把内容块拼成字符串。

    接收方向不要自己构造 `InboundEvent`：调用基类的 `self._build_event(sender_id=..., chat_id=..., text=...)`，它会执行 `allow_from` 白名单校验，并剥掉外部传入的 `_` 前缀内部控制键（如 `_cron_authorized`）。绕过它等于把审批闸门交给外部输入。

## 检查清单

- [ ] 继承 `BaseChannel`，实现 `start()`、`stop()`、`send()`
- [ ] 设置 `name`（唯一标识）
- [ ] 正确声明 `supports_edit` / `supports_files` / `is_realtime`
- [ ] Webhook 签名验证（安全性）
- [ ] 正确构造 `InboundEvent` 并发布到 bus
- [ ] `send()` 中调用 `self.should_deliver()` 过滤
- [ ] 返回正确的 `SendResult`（含 message_id 以支持编辑）
- [ ] 在 ChannelManager 注册
- [ ] 添加配置字段
- [ ] 资源清理（stop 中关闭 session）
- [ ] 编写单元测试

## 关于外部通道包

`echo_agent.channels.manager` 导出了 `register_channel_type(name, cls)`，可在运行时向注册表插入通道类型。但插件系统目前没有接线到这个入口——仓库内没有任何调用方，插件的 entry-point 也不会自动注册通道。

这意味着新增通道的实际路径仍是改动本仓库：把通道类加入 `_CHANNEL_REGISTRY`，并在配置 schema 中添加对应字段。第三方以独立包分发通道需要自行在包的初始化代码中调用 `register_channel_type()`，且要自行处理配置字段的注入——这条路径未经支持，也没有测试覆盖。
