# Using Plugins

Guide to installing and managing Echo Agent plugins.

---

## Overview

Plugins extend Echo Agent with additional tools, hooks, and integrations beyond the built-in capabilities.

## Configuration

```yaml
plugins:
  enabled: true
  allow: []            # allowlist (empty = allow all)
  deny: []             # blocklist
  extraDirs: []        # additional plugin directories
  trustedPlugins: []   # bypass manifest registration-permission checks
  permissionMode: compat  # strict | compat
  config:
    my-plugin:
      apiKey: "..."
```

## CLI Commands

```bash
echo-agent plugin list          # List all plugins
echo-agent plugin info <name>   # Show plugin details
echo-agent plugin enable <name>  # Enable a plugin
echo-agent plugin disable <name> # Disable a plugin
echo-agent plugin check          # Verify all plugins
```

## Installing Plugins

### From Directory

Place the plugin folder in the plugins directory or add to `extraDirs`:

```yaml
plugins:
  extraDirs: ["/path/to/my-plugins"]
```

### From Python Package

Install the package that declares the `echo_agent.plugins` entry point:

```bash
pip install echo-agent-plugin-example
```

## Allow/Deny Lists

Control which plugins load:

```yaml
plugins:
  allow: [trusted-plugin-1, trusted-plugin-2]  # only these load
  deny: [unwanted-plugin]  # these never load
```

A non-empty `allow` restricts loading to the listed plugins. When a plugin appears in both lists, **`deny` wins** — filtering checks the blocklist first and skips the plugin without consulting the allowlist.

!!! danger "Python plugins are trusted in-process code"
    Plugins run in the same Python process as Echo Agent. The permission mechanism enforces `tool.register` and `hook.register` when a plugin registers capabilities; `network`, `subprocess`, and `filesystem.*` are advisory manifest metadata, not an OS-level sandbox. Install only trusted plugins. Run untrusted code in a separate process or container and expose it through MCP. `trustedPlugins` only bypasses registration-permission checks; it does not add or remove code isolation.
