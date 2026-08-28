# 新增 Tool 指南

本指南介绍如何为 Echo Agent 开发新的工具（Tool），使 Agent 获得新能力。

## 架构概述

```
echo_agent/tools/
├── base.py              # Tool 抽象基类、ToolResult、ToolExecutionContext

echo_agent/agent/tools/
├── base.py              # 重导出（向后兼容）
├── registry.py          # ToolRegistry — 注册、执行、审计
├── shell.py             # 参考：ShellTool
├── filesystem.py        # 参考：文件系统工具
├── search.py            # 参考：搜索工具
└── your_tool.py         # ← 你的新工具
```

## Tool 基类

每个工具继承 `Tool` 并实现 `execute()` 方法：

```python
class Tool(ABC):
    name: str = ""                    # 工具名称（LLM 调用时使用）
    description: str = ""             # 功能描述（LLM 看到的）
    parameters: dict[str, Any] = {}   # JSON Schema 参数定义
    timeout_seconds: int = 30         # 执行超时
    max_retries: int = 0              # 自动重试次数
    stream_capable: bool = False      # 是否支持流式输出
    capabilities: tuple[str, ...] = () # 能力标签
    risk_level: str = "write"         # 风险等级：read / write / exec

    @abstractmethod
    async def execute(self, params: dict[str, Any], ctx: ToolExecutionContext | None = None) -> ToolResult:
        """执行工具逻辑。"""
```

## 步骤一：创建工具类

在 `echo_agent/agent/tools/` 下新建文件，例如 `weather.py`：

```python
"""Weather query tool — fetches current weather data."""

from __future__ import annotations

from typing import Any

import httpx
from loguru import logger

from echo_agent.tools.base import Tool, ToolExecutionContext, ToolResult


class WeatherTool(Tool):
    name = "weather"
    description = "Query current weather for a given city."
    risk_level = "read"  # 只读操作，无副作用
    timeout_seconds = 15
    capabilities = ("weather", "location")
    parameters = {
        "type": "object",
        "properties": {
            "city": {
                "type": "string",
                "description": "City name (e.g., 'Beijing', 'Tokyo')",
            },
            "units": {
                "type": "string",
                "enum": ["metric", "imperial"],
                "description": "Temperature units",
                "default": "metric",
            },
        },
        "required": ["city"],
    }

    def __init__(self, api_key: str = ""):
        self._api_key = api_key

    def is_ready(self) -> bool:
        """API Key 是否已配置。"""
        return bool(self._api_key)

    def readiness_detail(self) -> tuple[bool, str]:
        if not self._api_key:
            return False, "WEATHER_API_KEY not configured"
        return True, "ok"

    async def execute(self, params: dict[str, Any], ctx: ToolExecutionContext | None = None) -> ToolResult:
        city = params["city"]
        units = params.get("units", "metric")

        try:
            async with httpx.AsyncClient(timeout=10) as client:
                resp = await client.get(
                    "https://api.openweathermap.org/data/2.5/weather",
                    params={"q": city, "units": units, "appid": self._api_key},
                )
                resp.raise_for_status()
                data = resp.json()
        except httpx.TimeoutException:
            return ToolResult(
                success=False,
                error=f"Weather API timeout for city: {city}",
                error_kind="timeout",
            )
        except httpx.HTTPStatusError as e:
            if e.response.status_code == 404:
                return ToolResult(
                    success=False,
                    error=f"City not found: {city}",
                    error_kind="business",
                )
            return ToolResult(
                success=False,
                error=f"Weather API error: {e.response.status_code}",
                error_kind="dependency",
            )
        except Exception as e:
            logger.error("Weather tool error: {}", e)
            return ToolResult(
                success=False,
                error=str(e),
                error_kind="internal",
            )

        temp = data["main"]["temp"]
        desc = data["weather"][0]["description"]
        unit_label = "°C" if units == "metric" else "°F"
        return ToolResult(
            success=True,
            output=f"{city}: {temp}{unit_label}, {desc}",
            metadata={"raw": data},
        )
```

## 步骤二：注册工具

工具注册有两种方式：

### 方式 A：在 AgentLoop 初始化中注册（内置工具）

在 Agent 初始化逻辑中添加（通常在构建 ToolRegistry 的位置）：

```python
from echo_agent.agent.tools.weather import WeatherTool

registry.register(WeatherTool(api_key=config.weather_api_key))
```

### 方式 B：通过 capabilities 声明自动发现

工具的 `capabilities` 元组用于能力匹配。注册系统会根据配置自动加载具备特定能力的工具。

## ToolResult 规范

```python
@dataclass
class ToolResult:
    success: bool = True          # 是否成功
    output: str = ""              # 成功时的文本输出（LLM 看到的）
    error: str = ""               # 失败时的错误信息
    metadata: dict[str, Any] = {} # 结构化元数据（不直接暴露给 LLM）
    error_kind: str = ""          # 错误分类（影响熔断器）
```

### error_kind 分类

| 类别 | 说明 | 触发熔断 |
|------|------|---------|
| `validation` | 参数校验失败（LLM 传错参） | 否 |
| `business` | 业务性失败（记录不存在、权限不足） | 否 |
| `timeout` | 执行超时 | 是 |
| `dependency` | 下游依赖故障（网络、外部 API） | 是 |
| `internal` | 工具内部异常 | 是 |

只有 `timeout`/`dependency`/`internal` 属于基础设施故障，触发熔断器计数。

## 步骤三：定义 risk_level

| 等级 | 含义 | 审批行为 |
|------|------|---------|
| `read` | 只读，无副作用 | 通常自动通过 |
| `write` | 有副作用（写文件、发消息） | 根据安全策略可能需要审批 |
| `exec` | 执行外部命令/代码 | 通常需要人工审批 |

## 步骤四：编写测试

```python
"""tests/test_weather_tool.py"""

import pytest
from unittest.mock import AsyncMock, patch, MagicMock

from echo_agent.agent.tools.weather import WeatherTool


@pytest.fixture
def tool():
    return WeatherTool(api_key="test-key")


@pytest.mark.asyncio
async def test_weather_success(tool):
    mock_data = {
        "main": {"temp": 25.0},
        "weather": [{"description": "clear sky"}],
    }
    with patch("httpx.AsyncClient.get") as mock_get:
        mock_resp = MagicMock()
        mock_resp.json.return_value = mock_data
        mock_resp.raise_for_status = MagicMock()
        mock_get.return_value = mock_resp

        result = await tool.execute({"city": "Beijing"})
        assert result.success
        assert "25.0" in result.output
        assert "clear sky" in result.output


@pytest.mark.asyncio
async def test_weather_city_not_found(tool):
    import httpx
    with patch("httpx.AsyncClient.get") as mock_get:
        mock_resp = MagicMock()
        mock_resp.status_code = 404
        mock_resp.raise_for_status.side_effect = httpx.HTTPStatusError(
            "Not Found", request=MagicMock(), response=mock_resp
        )
        mock_get.return_value = mock_resp

        result = await tool.execute({"city": "NonexistentCity"})
        assert not result.success
        assert result.error_kind == "business"


def test_readiness_without_key():
    tool = WeatherTool(api_key="")
    assert not tool.is_ready()
    ready, msg = tool.readiness_detail()
    assert not ready
```

## Parameters JSON Schema

参数定义遵循 JSON Schema 规范，系统会自动校验：

```python
parameters = {
    "type": "object",
    "properties": {
        "query": {
            "type": "string",
            "description": "搜索关键词",
        },
        "limit": {
            "type": "integer",
            "description": "返回结果数量上限",
            "default": 10,
            "minimum": 1,
            "maximum": 100,
        },
        "tags": {
            "type": "array",
            "items": {"type": "string"},
            "description": "过滤标签",
        },
    },
    "required": ["query"],
}
```

## 检查清单

- [ ] 继承 `Tool`，实现 `execute()`
- [ ] 设置 `name`（唯一标识，LLM 调用时使用）
- [ ] 设置 `description`（清晰描述功能，LLM 据此决定何时使用）
- [ ] 定义 `parameters`（完整 JSON Schema，含 description）
- [ ] 设置合适的 `risk_level`
- [ ] 设置合理的 `timeout_seconds`
- [ ] 实现 `is_ready()` / `readiness_detail()`（如有外部依赖）
- [ ] 正确设置 `error_kind`（影响熔断器行为）
- [ ] 注册到 ToolRegistry
- [ ] 编写单元测试
- [ ] output 格式对 LLM 友好（简洁、结构化）

!!! note "工具注册是代码注册"
    工具通过代码注册到注册表，没有 YAML manifest 之类的声明式入口——参数 schema 由工具类自身给出。技能（Skill）才使用带 manifest 的目录结构，两者不是同一套机制，参见[技能编写](skill-authoring.md)。
