# 新增 Provider 指南

本指南介绍如何为 Echo Agent 接入新的 LLM 服务商（如 Mistral、Cohere、本地模型等）。

## 架构概述

```
echo_agent/models/
├── provider.py          # LLMProvider 抽象基类
├── providers/
│   ├── __init__.py      # 工厂函数 + _PROVIDER_MAP 注册表
│   ├── openai_provider.py   # 参考实现
│   └── your_provider.py     # ← 你的新 Provider
```

所有 Provider 继承 `LLMProvider`，实现 `chat()` 和 `get_default_model()` 方法。系统通过 `_PROVIDER_MAP` 字典按名称路由到具体实现。

## 步骤一：创建 Provider 类

在 `echo_agent/models/providers/` 下新建文件，例如 `mistral_provider.py`：

```python
"""Mistral provider — chat completions via the Mistral SDK."""

from __future__ import annotations

from typing import Any

from loguru import logger

from echo_agent.models.provider import (
    LLMProvider,
    LLMResponse,
    StreamDeltaCallback,
    StreamReasoningCallback,
    ToolCallRequest,
    _invoke_stream_callback,
)


class MistralProvider(LLMProvider):

    def __init__(self, api_key: str = "", api_base: str = "", default_model: str = "", **kwargs: Any):
        super().__init__(api_key=api_key, api_base=api_base)
        self._default_model = default_model or "mistral-large-latest"
        self._client = self._build_client()

    def _build_client(self) -> Any:
        try:
            from mistralai import Mistral
        except ImportError:
            raise ImportError("mistral SDK required: pip install mistralai")
        return Mistral(api_key=self.api_key)

    async def chat(
        self,
        messages: list[dict[str, Any]],
        tools: list[dict[str, Any]] | None = None,
        model: str | None = None,
        tool_choice: str | dict | None = None,
        **kwargs: Any,
    ) -> LLMResponse:
        """发送聊天补全请求。"""
        # 构建请求参数
        params: dict[str, Any] = {
            "model": model or self._default_model,
            "messages": messages,
        }
        if tools:
            params["tools"] = tools
        if tool_choice:
            params["tool_choice"] = tool_choice

        try:
            resp = await self._client.chat.complete_async(**params)
        except Exception as e:
            logger.error("Mistral API error: {}", e)
            return LLMResponse(content=f"Error: {e}", finish_reason="error")

        return self._parse_response(resp)

    def get_default_model(self) -> str:
        return self._default_model

    async def chat_stream(
        self,
        messages: list[dict[str, Any]],
        tools: list[dict[str, Any]] | None = None,
        model: str | None = None,
        tool_choice: str | dict | None = None,
        on_delta: StreamDeltaCallback | None = None,
        on_reasoning: StreamReasoningCallback | None = None,
        **kwargs: Any,
    ) -> LLMResponse:
        """流式聊天补全。"""
        # 实现流式响应...
        pass

    def _parse_response(self, resp: Any) -> LLMResponse:
        """将 SDK 响应转为统一的 LLMResponse。"""
        choice = resp.choices[0]
        tool_calls = []
        if choice.message.tool_calls:
            for tc in choice.message.tool_calls:
                tool_calls.append(ToolCallRequest(
                    id=tc.id,
                    name=tc.function.name,
                    arguments=tc.function.arguments if isinstance(tc.function.arguments, dict)
                              else {},
                ))
        return LLMResponse(
            content=choice.message.content or "",
            tool_calls=tool_calls,
            finish_reason=choice.finish_reason or "stop",
            usage={
                "input_tokens": resp.usage.prompt_tokens,
                "output_tokens": resp.usage.completion_tokens,
            },
            model=resp.model or self._default_model,
        )

    async def embed(self, text: str, model: str | None = None) -> list[float] | None:
        """可选：实现嵌入接口。"""
        return None
```

## 步骤二：注册到 Provider Map

编辑 `echo_agent/models/providers/__init__.py`，在 `_PROVIDER_MAP` 中添加映射：

```python
_PROVIDER_MAP: dict[str, str] = {
    "openai": "echo_agent.models.providers.openai_provider.OpenAIProvider",
    "anthropic": "echo_agent.models.providers.anthropic_provider.AnthropicProvider",
    "bedrock": "echo_agent.models.providers.bedrock_provider.BedrockProvider",
    "aws": "echo_agent.models.providers.bedrock_provider.BedrockProvider",
    "gemini": "echo_agent.models.providers.gemini_provider.GeminiProvider",
    "google": "echo_agent.models.providers.gemini_provider.GeminiProvider",
    "openrouter": "echo_agent.models.providers.openrouter_provider.OpenRouterProvider",
    # ← 新增
    "mistral": "echo_agent.models.providers.mistral_provider.MistralProvider",
}
```

如果需要环境变量自动发现 API Key，也添加到 `_API_KEY_ENV`：

```python
_API_KEY_ENV: dict[str, tuple[str, ...]] = {
    ...
    "mistral": ("MISTRAL_API_KEY",),
}
```

## 步骤三：添加可选依赖

在 `pyproject.toml` 中声明：

```toml
[project.optional-dependencies]
mistral = ["mistralai>=1.0"]
```

并将其加入 `all` 集合和 `allproviders`。

## 步骤四：编写测试

在 `tests/` 下创建测试文件：

```python
"""tests/test_mistral_provider.py"""

import pytest
from unittest.mock import AsyncMock, patch, MagicMock

from echo_agent.models.providers.mistral_provider import MistralProvider


@pytest.fixture
def provider():
    with patch("echo_agent.models.providers.mistral_provider.MistralProvider._build_client"):
        p = MistralProvider(api_key="test-key")
        p._client = MagicMock()
        return p


@pytest.mark.asyncio
async def test_chat_success(provider):
    mock_resp = MagicMock()
    mock_resp.choices = [MagicMock()]
    mock_resp.choices[0].message.content = "Hello!"
    mock_resp.choices[0].message.tool_calls = None
    mock_resp.choices[0].finish_reason = "stop"
    mock_resp.usage.prompt_tokens = 10
    mock_resp.usage.completion_tokens = 5
    mock_resp.model = "mistral-large-latest"

    provider._client.chat.complete_async = AsyncMock(return_value=mock_resp)

    result = await provider.chat([{"role": "user", "content": "Hi"}])
    assert result.content == "Hello!"
    assert result.finish_reason == "stop"


@pytest.mark.asyncio
async def test_chat_error(provider):
    provider._client.chat.complete_async = AsyncMock(side_effect=Exception("API down"))
    result = await provider.chat([{"role": "user", "content": "Hi"}])
    assert result.finish_reason == "error"
```

## 必须实现的方法

| 方法 | 必须 | 说明 |
|------|------|------|
| `chat()` | 是 | 非流式聊天补全 |
| `get_default_model()` | 是 | 返回默认模型标识 |
| `chat_stream()` | 否 | 流式响应（不实现则自动降级为非流式） |
| `embed()` | 否 | 嵌入向量生成 |
| `supports_embed()` | 否 | 声明是否支持 embed |
| `aclose()` | 否 | 清理 SDK 客户端资源 |

## LLMResponse 字段规范

```python
@dataclass
class LLMResponse:
    content: str | None = None          # 文本响应
    tool_calls: list[ToolCallRequest]   # 工具调用请求
    finish_reason: str = "stop"         # stop / tool_calls / error / length
    usage: dict[str, int] = {}          # input_tokens, output_tokens, cache_read_input_tokens
    model: str = ""                     # 实际使用的模型
    reasoning_content: str | None = None  # 推理内容（如 Claude 的 thinking）
```

## 检查清单

- [ ] 继承 `LLMProvider`，实现 `chat()` + `get_default_model()`
- [ ] 在 `_PROVIDER_MAP` 注册
- [ ] 在 `_API_KEY_ENV` 注册（如适用）
- [ ] `pyproject.toml` 添加可选依赖
- [ ] SDK 通过延迟 import 引入（`ImportError` 提示安装方式）
- [ ] 错误处理：API 异常返回 `finish_reason="error"`，不抛异常
- [ ] 编写单元测试（mock SDK 调用）
- [ ] Tool calls 正确转为 `ToolCallRequest` 格式
- [ ] Usage 统计正确填充（影响成本追踪）

!!! question "需维护者确认"
    OpenAI-compatible 模式（未在 `_PROVIDER_MAP` 中注册的 provider 自动降级为 OpenAI SDK 调用）是否需要文档化为正式的"自定义 Provider"入口？
