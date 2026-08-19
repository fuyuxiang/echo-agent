# Plugin API

Echo Agent's plugin system allows third-party extensions to expand the Agent's capabilities, including adding tools, registering lifecycle hooks, and providing new commands.

## Plugin Structure

```
my-echo-plugin/
├── plugin.yaml          # Plugin manifest (required)
├── __init__.py          # Entry module
├── tools.py             # Tool implementations (optional)
├── hooks.py             # Hook implementations (optional)
└── pyproject.toml       # Python package configuration
```

## plugin.yaml Manifest

```yaml
name: my-awesome-plugin
version: 1.0.0
description: "A plugin that adds weather lookup and notification features"
author: "Your Name"
license: "MIT"

# Echo Agent version requirement
requires_echo_agent: ">=0.3.0"

# Required environment variables (checked at startup)
requires_env:
  - WEATHER_API_KEY

# Capabilities provided by this plugin
provides:
  tools:
    - weather_lookup
    - weather_forecast
  hooks:
    - on_agent_start
    - post_tool_call
  commands: []

# Plugin type: integration / extension / theme
kind: integration

# Config key (namespace in echo-agent configuration)
config_key: weather

# Other plugins this depends on
depends_on: []

# Required permissions
permissions:
  - network    # Network access
  - filesystem # Filesystem access (restricted)
```

## PluginManifest Fields

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `name` | str | Yes | Unique plugin identifier |
| `version` | str | No | Semantic version (default 0.0.1) |
| `description` | str | No | Plugin description |
| `author` | str | No | Author |
| `license` | str | No | License |
| `requires_echo_agent` | str | No | Compatible Echo Agent version range |
| `requires_env` | list[str] | No | Required environment variables |
| `provides.tools` | list[str] | No | List of provided tools |
| `provides.hooks` | list[str] | No | List of registered hooks |
| `provides.commands` | list[str] | No | Provided commands |
| `kind` | str | No | Type: integration / extension / theme |
| `config_key` | str | No | Configuration namespace |
| `depends_on` | list[str] | No | Other plugins this depends on |
| `permissions` | list[str] | No | Required permissions |

## Lifecycle Hooks

Plugins can register the following lifecycle hooks:

```python
VALID_HOOKS = {
    "on_agent_start",    # Agent startup
    "on_agent_stop",     # Agent shutdown
    "on_session_start",  # Session begins
    "on_session_end",    # Session ends
    "pre_tool_call",     # Before tool call (can cancel)
    "post_tool_call",    # After tool call (can modify result)
    "pre_llm_call",      # Before LLM call
    "post_llm_call",     # After LLM call
    "pre_approval",      # Before approval
    "post_approval",     # After approval
    "on_error",          # When an error occurs
}
```

### Hook Implementation Example

```python
"""hooks.py — Plugin lifecycle hooks."""

from echo_agent.plugins.hooks import HookResult


async def on_agent_start(**kwargs) -> HookResult | None:
    """Initialize plugin resources when Agent starts."""
    # Initialize connection pools, load caches, etc.
    return None


async def pre_tool_call(tool_name: str, params: dict, **kwargs) -> HookResult | None:
    """Intercept before tool call."""
    # Can modify parameters or cancel the call
    if tool_name == "exec" and "rm -rf" in params.get("command", ""):
        return HookResult(cancel=True, cancel_reason="Dangerous command blocked by plugin")
    return None


async def post_tool_call(tool_name: str, result: dict, **kwargs) -> HookResult | None:
    """Process after tool call."""
    # Can modify results or trigger additional actions
    if tool_name == "weather_lookup":
        # Record weather query history
        pass
    return None


async def on_error(error: Exception, **kwargs) -> HookResult | None:
    """Notification when error occurs."""
    # Send alerts, record logs, etc.
    return None
```

### HookResult

```python
@dataclass
class HookResult:
    modified: Any = None           # Modified data (passed to next hook/caller)
    stop_propagation: bool = False # Prevent subsequent hooks from executing
    cancel: bool = False           # Cancel current operation
    cancel_reason: str = ""        # Cancellation reason
```

## Registration Methods

### Method 1: Entry Points (recommended for PyPI distribution)

Declare in your plugin's `pyproject.toml`:

```toml
[project.entry-points."echo_agent.plugins"]
my-awesome-plugin = "my_plugin"
```

Echo Agent automatically discovers all packages registered with the `echo_agent.plugins` entry point at startup.

### Method 2: User Directory Installation

Place the plugin directory in the user config path:

```
~/.echo-agent/plugins/my-awesome-plugin/
├── plugin.yaml
└── __init__.py
```

### Method 3: Project Directory Installation

In the project working directory:

```
.echo-agent/plugins/my-awesome-plugin/
├── plugin.yaml
└── __init__.py
```

## Plugin Entry Module

`__init__.py` is the Python entry point, exporting tools and hooks:

```python
"""my_plugin — Weather integration for Echo Agent."""

from my_plugin.tools import WeatherLookupTool, WeatherForecastTool
from my_plugin.hooks import on_agent_start, pre_tool_call, post_tool_call

# Auto-registered when plugin loads
TOOLS = [WeatherLookupTool, WeatherForecastTool]
HOOKS = {
    "on_agent_start": on_agent_start,
    "pre_tool_call": pre_tool_call,
    "post_tool_call": post_tool_call,
}


async def activate(context):
    """Plugin activation callback."""
    pass


async def deactivate(context):
    """Plugin deactivation callback."""
    pass
```

## Plugin State Lifecycle

```
discovered → loaded → activated → (running)
                ↓                      ↓
              failed               disabled
```

| State | Description |
|-------|-------------|
| `discovered` | plugin.yaml found |
| `loaded` | Module imported successfully |
| `activated` | activate() succeeded, tools/hooks registered |
| `failed` | Loading or activation failed |
| `disabled` | Manually disabled by user |

## Sandbox and Permissions

Plugins run in a restricted environment:

- **Network access** — Requires `network` permission declaration
- **Filesystem** — Requires `filesystem` permission, path-restricted
- **Tool calls** — Plugin-registered tools follow the same approval flow as built-in tools
- **Resource limits** — Execution timeout and memory limits controlled by sandbox

## Development Workflow

### 1. Initialize Plugin Project

```bash
mkdir my-echo-plugin && cd my-echo-plugin
```

### 2. Create plugin.yaml

Define plugin metadata and capability declarations.

### 3. Implement Tools/Hooks

Implement Tool classes and hook functions as needed.

### 4. Local Testing

```bash
# Symlink plugin directory to user plugin path
ln -s $(pwd) ~/.echo-agent/plugins/my-echo-plugin

# Start Echo Agent, observe plugin loading logs
echo-agent --log-level DEBUG
```

### 5. Package and Publish

```bash
pip install build
python -m build
pip install twine
twine upload dist/*
```

## Checklist

- [ ] `plugin.yaml` manifest is complete and valid
- [ ] `requires_env` correctly declared (clear error when missing)
- [ ] `provides` accurately lists all tools and hooks
- [ ] Hook functions accept `**kwargs` (forward-compatible with new parameters)
- [ ] `activate()` / `deactivate()` properly manage resources
- [ ] Errors don't leak to host (exceptions caught at plugin boundary)
- [ ] Entry point correctly registered (if PyPI distribution needed)
- [ ] Local installation test passes

!!! question "Pending maintainer confirmation"
    Is there a plan to provide an `echo-agent plugin init` scaffold command? Currently all files must be created manually.
