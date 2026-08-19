# Adding a Tool

This guide explains how to develop new tools for Echo Agent, giving the Agent new capabilities.

## Architecture Overview

```
echo_agent/tools/
├── base.py              # Tool abstract base class, ToolResult, ToolExecutionContext

echo_agent/agent/tools/
├── base.py              # Re-export (backward compatibility)
├── registry.py          # ToolRegistry — registration, execution, audit
├── shell.py             # Reference: ShellTool
├── filesystem.py        # Reference: Filesystem tools
├── search.py            # Reference: Search tool
└── your_tool.py         # ← Your new tool
```

## Tool Base Class

Each tool inherits from `Tool` and implements the `execute()` method:

```python
class Tool(ABC):
    name: str = ""                    # Tool name (used by LLM to invoke)
    description: str = ""             # Functionality description (visible to LLM)
    parameters: dict[str, Any] = {}   # JSON Schema parameter definition
    timeout_seconds: int = 30         # Execution timeout
    max_retries: int = 0              # Automatic retry count
    stream_capable: bool = False      # Whether streaming output is supported
    capabilities: tuple[str, ...] = () # Capability tags
    risk_level: str = "write"         # Risk level: read / write / exec

    @abstractmethod
    async def execute(self, params: dict[str, Any], ctx: ToolExecutionContext | None = None) -> ToolResult:
        """Execute tool logic."""
```

## Step 1: Create the Tool Class

Create a new file under `echo_agent/agent/tools/`, e.g., `weather.py`:

```python
"""Weather query tool — fetches current weather data."""

from __future__ import annotations

from typing import Any

import httpx
from loguru import logger

from echo_agent.agent.tools.base import Tool, ToolExecutionContext, ToolResult


class WeatherTool(Tool):
    name = "weather"
    description = "Query current weather for a given city."
    risk_level = "read"  # Read-only, no side effects
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
        """Whether the API Key is configured."""
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

## Step 2: Register the Tool

There are two ways to register a tool:

### Option A: Register during AgentLoop initialization (built-in tools)

Add in the Agent initialization logic (where ToolRegistry is built):

```python
from echo_agent.agent.tools.weather import WeatherTool

registry.register(WeatherTool(api_key=config.weather_api_key))
```

### Option B: Auto-discovery via capabilities declaration

The `capabilities` tuple is used for capability matching. The registration system can automatically load tools with specific capabilities based on configuration.

## ToolResult Specification

```python
@dataclass
class ToolResult:
    success: bool = True          # Whether execution succeeded
    output: str = ""              # Text output on success (visible to LLM)
    error: str = ""               # Error message on failure
    metadata: dict[str, Any] = {} # Structured metadata (not directly exposed to LLM)
    error_kind: str = ""          # Error classification (affects circuit breaker)
```

### error_kind Categories

| Category | Description | Triggers Circuit Breaker |
|----------|-------------|-------------------------|
| `validation` | Parameter validation failure (LLM passed wrong params) | No |
| `business` | Business failure (record not found, permission denied) | No |
| `timeout` | Execution timeout | Yes |
| `dependency` | Downstream dependency failure (network, external API) | Yes |
| `internal` | Internal tool exception | Yes |

Only `timeout`/`dependency`/`internal` are infrastructure failures that trigger circuit breaker counting.

## Step 3: Define risk_level

| Level | Meaning | Approval Behavior |
|-------|---------|-------------------|
| `read` | Read-only, no side effects | Usually auto-approved |
| `write` | Has side effects (write files, send messages) | May require approval per security policy |
| `exec` | Executes external commands/code | Usually requires human approval |

## Step 4: Write Tests

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

Parameter definitions follow the JSON Schema spec; the system validates automatically:

```python
parameters = {
    "type": "object",
    "properties": {
        "query": {
            "type": "string",
            "description": "Search keywords",
        },
        "limit": {
            "type": "integer",
            "description": "Maximum number of results to return",
            "default": 10,
            "minimum": 1,
            "maximum": 100,
        },
        "tags": {
            "type": "array",
            "items": {"type": "string"},
            "description": "Filter tags",
        },
    },
    "required": ["query"],
}
```

## Checklist

- [ ] Inherit `Tool`, implement `execute()`
- [ ] Set `name` (unique identifier, used by LLM to invoke)
- [ ] Set `description` (clear functionality description, LLM uses this to decide when to call)
- [ ] Define `parameters` (complete JSON Schema with descriptions)
- [ ] Set appropriate `risk_level`
- [ ] Set reasonable `timeout_seconds`
- [ ] Implement `is_ready()` / `readiness_detail()` (if external dependencies exist)
- [ ] Set `error_kind` correctly (affects circuit breaker behavior)
- [ ] Register in ToolRegistry
- [ ] Write unit tests
- [ ] Output format is LLM-friendly (concise, structured)

!!! question "Pending maintainer confirmation"
    Is there a plan to migrate tool registration to declarative configuration (e.g., YAML manifest), or will the code-based registration approach be maintained?
