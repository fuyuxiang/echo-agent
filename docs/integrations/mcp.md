# MCP (Model Context Protocol)

通过 MCP 协议连接外部工具服务器，扩展 Agent 的工具能力。

---

## 概述

MCP 是一种标准化的 AI 工具通信协议。Echo Agent 作为 MCP 客户端，可以连接任意 MCP Server 来获得额外工具能力。

## 配置

```yaml
tools:
  mcp_servers:
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

## 传输方式

### Stdio（子进程）

通过标准输入/输出与 MCP Server 通信：

```yaml
tools:
  mcp_servers:
    my-server:
      command: "python"
      args: ["-m", "my_mcp_server"]
      env:
        API_KEY: "${MY_API_KEY}"
```

### HTTP/SSE（远程）

通过 HTTP 连接远程 MCP Server：

```yaml
tools:
  mcp_servers:
    remote:
      url: "https://mcp.example.com/sse"
      headers:
        Authorization: "Bearer token"
      auth: "oauth"  # 可选 OAuth 支持
```

## 工具过滤

```yaml
tools:
  mcp_servers:
    my-server:
      command: "..."
      tools_include: ["read_file", "write_file"]  # 白名单
      tools_exclude: ["dangerous_tool"]           # 黑名单
```

## 安全

- MCP 工具经过 `validate_mcp_tools()` 安全检查
- 与内置工具同名的 MCP 工具会被拒绝
- 受 `tools.profile` 和 `permissions.approval.mode` 约束

## 重连机制

连接断开时自动指数退避重连（1, 2, 4, 8, 16, 30, 60 秒），最多 5 次尝试。

## 协议版本

- JSON-RPC 2.0
- MCP 协议版本：2024-11-05
- 支持：tools/list, tools/call, resources/list, resources/read, prompts/list, prompts/get
