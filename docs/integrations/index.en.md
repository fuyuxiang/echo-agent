# Integrations & Extensions

Echo Agent integrates with external systems through a multi-layer extension architecture covering message channels, API gateway, skill system, plugin mechanism, and multi-agent protocols.

## Architecture Overview

```
┌─────────────────────────────────────────────────┐
│                  Echo Agent Core                  │
├──────────┬──────────┬──────────┬────────────────┤
│ Channels │ Gateway  │  Skills  │    Plugins     │
│  (14)    │ HTTP/WS  │  (35)    │  plugin.yaml   │
├──────────┴──────────┴──────────┴────────────────┤
│       MCP (Tool Extension) │ A2A (Multi-Agent)  │
└─────────────────────────────────────────────────┘
```

## Module Guide

| Module | Description | Documentation |
|--------|-------------|---------------|
| [Channels](channels/index.md) | 14 platform adapters covering IM, email, webhook, CLI | Configuration & capability matrix |
| [Gateway](gateway/index.md) | HTTP/WebSocket API gateway with auth, rate limiting, sessions | Auth modes & reverse proxy |
| [Skills](skills/using-skills.md) | 35 built-in skills organized by category | Usage & catalog |
| [Plugins](plugins/using-plugins.md) | Extension mechanism based on plugin.yaml | Usage & development |
| [MCP](mcp.md) | Model Context Protocol client for external tool servers | Configuration & usage |
| [A2A](a2a.md) | Agent-to-Agent protocol for multi-agent task delegation | Protocol & integration |

## Quick Start

### Enable a Channel

Enable a channel in `config.yaml`:

```yaml
channels:
  telegram:
    enabled: true
    token: "YOUR_BOT_TOKEN"
```

### Connect an MCP Tool Server

```yaml
mcp:
  servers:
    filesystem:
      enabled: true
      command: "npx"
      args: ["-y", "@modelcontextprotocol/server-filesystem", "/path/to/dir"]
```

### Enable A2A Protocol

```yaml
a2a:
  enabled: true
  agent_card:
    name: "my-agent"
    description: "My Echo Agent instance"
```

## Design Principles

- **Unified Message Bus** — All channels communicate through `MessageBus`, fully decoupled
- **Self-describing Capabilities** — Each channel declares its capabilities (edit, reactions, files); routing adapts accordingly
- **Progressive Enablement** — All integrations are off by default; enable as needed. CLI mode works with zero config
- **Security First** — Independent auth at each layer: channel allowlists, Gateway tokens, plugin permission sandboxes
