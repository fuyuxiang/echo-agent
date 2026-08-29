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
│       MCP (Tool Extension) │ A2A (Inbound Tasks) │
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
| [A2A](a2a.md) | Inbound Agent-to-Agent protocol for text tasks delegated to Echo Agent by external peers | Protocol & integration |

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

MCP servers are configured under `tools.mcp_servers` (not a top-level `mcp` section), keyed by server name:

```yaml
tools:
  mcp:
    enabled: true
  mcp_servers:
    filesystem:
      enabled: true
      command: "npx"
      args: ["-y", "@modelcontextprotocol/server-filesystem", "/path/to/dir"]
```

### Enable A2A Protocol

The `a2a` fields are flat — there is no `agent_card` subsection:
Enabling it exposes the inbound `/a2a` endpoint; it does not register an outbound delegation tool.

```yaml
a2a:
  enabled: true
  agent_name: "my-agent"
  agent_description: "My Echo Agent instance"
```

## Design Principles

- **Unified Message Bus** — All channels communicate through `MessageBus`, fully decoupled
- **Self-describing Capabilities** — Each channel declares its capabilities (edit, reactions, files); routing adapts accordingly
- **Progressive Enablement** — All integrations are off by default; enable as needed. CLI mode works with zero config
- **Explicit Security Boundaries** — Channel allowlists and Gateway tokens authenticate ingress. Plugin permission declarations govern registration admission only; Python plugins are trusted in-process code, not OS-sandboxed programs
