# Plugin API

Echo Agent's plugin system allows third-party extensions to add tools and register lifecycle hooks. `provides.commands` is reserved for a future extension; the current runtime does not register plugin commands.

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
  - tool.register # Allow tool registration
  - hook.register # Allow hook registration
  - network       # Advisory network-access metadata
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
| `provides.commands` | list[str] | No | Reserved; commands are not registered by the current runtime |
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

`__init__.py` is the Python entry point. The runtime resolves only `activate(context)` and optional `deactivate(context)`; tools and hooks must be registered explicitly inside `activate`:

```python
"""my_plugin — Weather integration for Echo Agent."""

from my_plugin.tools import WeatherLookupTool, WeatherForecastTool
from my_plugin.hooks import on_agent_start, pre_tool_call, post_tool_call

async def activate(context):
    """Plugin activation callback."""
    context.register_tools([WeatherLookupTool(), WeatherForecastTool()])
    context.register_hook("on_agent_start", on_agent_start)
    context.register_hook("pre_tool_call", pre_tool_call)
    context.register_hook("post_tool_call", post_tool_call)


async def deactivate(context):
    """Plugin deactivation callback."""
    # Close non-tool resources owned by the plugin; PluginManager removes registrations.
    ...
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

## Trust Model and Permission Declarations

Python plugins run as trusted code inside the Echo Agent process. The current mechanism is not an OS-level sandbox:

- **Registration permissions** — `tool.register` and `hook.register` are enforced when a plugin registers capabilities
- **Advisory permissions** — `network`, `subprocess`, and `filesystem.*` document intent but do not prevent in-process Python code from accessing those resources directly
- **Tool approval** — Plugin-registered tools follow the same approval flow as built-in tools
- **Untrusted-code isolation** — Run it in a separate process or container and expose it through MCP

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

There is no scaffold command; plugin files are created by hand. `echo-agent plugin` supports `list`, `info`, `enable`, `disable` and `check`, the last of which verifies that a finished plugin loads correctly.
