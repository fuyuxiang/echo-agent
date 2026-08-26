# Reference Documentation

This section provides complete technical reference for Echo Agent v0.3.8 Beta. Each page documents a specific subsystem with precise specifications, option tables, and usage examples.

!!! tip "Looking for tutorials?"
    Reference pages assume familiarity with Echo Agent concepts. For getting-started guides, see the [Quickstart](../getting-started/quickstart.en.md) section.

## Reference Pages

| Page | Description |
|------|-------------|
| [CLI Commands](cli.en.md) | All `echo-agent` command-line commands, flags, and subcommands |
| [TUI Commands](tui-commands.en.md) | Interactive terminal UI slash commands (local and server-side) |
| [Configuration Guide](configuration-guide.en.md) | YAML configuration structure, loading order, and field reference |
| [Environment Variables](environment-variables.en.md) | `ECHO_AGENT_` environment variable mapping and override rules |
| [Built-in Tools](tools.en.md) | All 30 built-in tools with parameters, risk levels, and examples |
| [Security Profile Matrix](security-profile-matrix.en.md) | Security and tool profile levels, permissions, and approval modes |
| [Gateway API](gateway-api.en.md) | REST API endpoints, authentication, request/response schemas |
| [WebSocket Protocol](websocket-protocol.en.md) | WebSocket message format, events, and connection lifecycle |
| [Filesystem Layout](filesystem-layout.en.md) | Data directories, file locations, and storage structure |
| [Compatibility](compatibility.en.md) | Platform support, Python versions, and dependency matrix |
| [Glossary](glossary.en.md) | Definitions of key terms and concepts used throughout the docs |

## Version Information

| Property | Value |
|----------|-------|
| Version | v0.3.8 Beta |
| Python | 3.11+ |
| Platforms | Linux, macOS, Windows (WSL2 recommended) |
| License | See repository root |

!!! warning "Beta Software"
    Echo Agent is in active development. APIs and configuration fields may change between minor versions. Pin your version in production deployments and review changelogs before upgrading.
