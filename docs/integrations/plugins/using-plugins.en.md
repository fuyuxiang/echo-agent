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
  trustedPlugins: []   # bypass sandbox checks
  permissionMode: compat  # strict | compat | legacy
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
