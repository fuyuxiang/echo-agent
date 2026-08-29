# Developing Plugins

Guide to creating Echo Agent plugins.

---

## Plugin Structure

```
my-plugin/
├── plugin.yaml          # Manifest (required)
├── __init__.py          # Entry point
└── tools/
    └── my_tool.py       # Custom tools
```

## Manifest (plugin.yaml)

```yaml
name: my-plugin
version: "1.0.0"
description: "What this plugin does"
author: "Your Name"
requires_env: [MY_API_KEY]
provides:
  tools: [my_custom_tool]
  hooks: [pre_tool_call, post_tool_call]
kind: integration
config_key: my_plugin
depends_on: []
permissions:
  - tool.register
  - hook.register
```

## Entry Point

```python
from echo_agent.plugins import PluginContext

async def activate(ctx: PluginContext):
    """Called when plugin loads."""
    # Register tools
    ctx.register_tool(MyTool(ctx.plugin_config))
    
    # Register hooks
    ctx.register_hook("pre_tool_call", my_hook)

async def deactivate(ctx: PluginContext):
    """Called on shutdown."""
    pass
```

## PluginContext

Available via `ctx`:

- `ctx.config` — Global Echo Agent config
- `ctx.workspace` — Workspace path
- `ctx.publish_outbound(...)` — Publish outbound events
- `ctx.subscribe_inbound(...)` — Subscribe to inbound events; automatically removed on deactivation
- `ctx.register_tool(...)` — Register a tool; PluginManager reclaims it on deactivation
- `ctx.register_hook(...)` — Register a lifecycle hook
- `ctx.plugin_config` — Plugin-specific config from `plugins.config.{config_key}`

## Permission Modes

| Mode | Behavior |
|------|----------|
| `strict` | Reject tool/hook registration without the matching declaration |
| `compat` | Give declaration-free legacy plugins default tool/hook registration permissions; explicit declarations are still checked |

!!! warning "Permission declarations are not process isolation"
    Python plugins are trusted in-process code. Only `tool.register` and `hook.register` are enforced at registration; `network`, `subprocess`, and `filesystem.*` are advisory metadata.

## Distribution

Plugins can be distributed as:

1. **Local directory** — place in plugins dir or `plugins.extraDirs`
2. **Python package** — register via `[project.entry-points."echo_agent.plugins"]` in pyproject.toml

## CLI Management

```bash
echo-agent plugin list
echo-agent plugin info <name>
echo-agent plugin enable <name>
echo-agent plugin disable <name>
echo-agent plugin check
```
