# MCP (Model Context Protocol)

Connect external tool servers via MCP protocol to extend Agent capabilities.

---

## Overview

MCP is a standardized AI tool communication protocol. Echo Agent acts as an MCP client, connecting to any MCP Server for additional tool capabilities.

## Configuration

```yaml
tools:
  mcpServers:
    filesystem:
      command: "npx"
      args: ["-y", "@modelcontextprotocol/server-filesystem", "/path/to/dir"]
      enabled: true
    
    web-search:
      url: "http://localhost:8080/mcp"
      headers:
        Authorization: "Bearer ${MCP_TOKEN}"
      enabled: true
```

## Transports

### Stdio (subprocess)

```yaml
tools:
  mcpServers:
    my-server:
      command: "python"
      args: ["-m", "my_mcp_server"]
      env:
        API_KEY: "${MY_API_KEY}"
```

### HTTP/SSE (remote)

```yaml
tools:
  mcpServers:
    remote:
      url: "https://mcp.example.com/sse"
      headers:
        Authorization: "Bearer token"
      auth: "oauth"
```

## Tool Filtering

```yaml
tools:
  mcpServers:
    my-server:
      command: "..."
      toolsInclude: ["read_file", "write_file"]
      toolsExclude: ["dangerous_tool"]
```

## Security

- MCP tools validated via `validate_mcp_tools()`
- MCP tools with names conflicting built-ins are rejected
- Subject to `tools.profile` and `permissions.approval.mode`

## Reconnection

Automatic exponential backoff (1, 2, 4, 8, 16, 30, 60s), max 5 attempts.

## Protocol

- JSON-RPC 2.0, protocol version 2024-11-05
- Capabilities: tools/list, tools/call, resources/list, resources/read, prompts/list, prompts/get
