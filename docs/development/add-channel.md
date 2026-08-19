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

在 `echo_agent/channels/manager.py` 的通道映射中注册：

```python
from echo_agent.channels.line import LineChannel

_CHANNEL_MAP: dict[str, type[BaseChannel]] = {
    "cli": CliChannel,
    "telegram": TelegramChannel,
    "discord": DiscordChannel,
    ...
    "line": LineChannel,  # ← 新增
}
```

## 步骤三：添加配置项

在 `echo_agent/config/schema.py` 中添加通道配置字段：

```python
class ChannelsConfig(BaseModel):
    ...
    line_channel_token: str = ""
    line_channel_secret: str = ""
    line_enabled: bool = False
```

## 步骤四：实现可选功能

### 消息编辑（如平台支持）

```python
async def edit_message(self, chat_id: str, message_id: str, new_text: str) -> bool:
    """编辑已发送的消息。"""
    # 实现平台的消息编辑 API
    return True
```

### 文件发送

```python
async def send_file(self, chat_id: str, file_path: str, caption: str = "") -> SendResult:
    """发送文件/图片。"""
    # 实现文件上传 API
    pass
```

### 表情回应

```python
async def add_reaction(self, chat_id: str, message_id: str, emoji: str) -> bool:
    """对消息添加表情。"""
    pass
```

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
from unittest.mock import AsyncMock, MagicMock, patch

from echo_agent.channels.line import LineChannel
from echo_agent.bus.events import OutboundEvent


@pytest.fixture
def channel():
    config = MagicMock()
    config.line_channel_token = "test-token"
    config.line_channel_secret = "test-secret"
    config.transcription_api_key = ""
    bus = MagicMock()
    return LineChannel(config, bus)


@pytest.mark.asyncio
async def test_send_success(channel):
    channel._session = MagicMock()
    mock_resp = MagicMock()
    mock_resp.status = 200
    channel._session.post = MagicMock(return_value=AsyncMock(__aenter__=AsyncMock(return_value=mock_resp)))

    event = OutboundEvent(chat_id="user123", text="Hello", is_final=True)
    result = await channel.send(event)
    assert result.success


@pytest.mark.asyncio
async def test_send_skipped_non_final(channel):
    event = OutboundEvent(chat_id="user123", text="thinking...", is_final=False)
    result = await channel.send(event)
    assert result.skipped  # supports_edit=False 跳过非 final
```

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

!!! question "需维护者确认"
    通道注册是否计划支持动态加载（类似 Plugin 机制），允许第三方通道作为独立包安装？
