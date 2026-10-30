# MCP (Model Context Protocol)

**Scope: standalone Python Runtime.** For Desktop operations, see the [Desktop guide](../desktop/index.md).

Echo Agent acts as an MCP client for external tool servers. The current implementation requests protocol version `2025-06-18` and can negotiate `2025-03-26` or `2024-11-05`. It supports stdio and Streamable HTTP transport, tool listing and calls, resources and prompts, and reconnects. Sampling, elicitation, roots, and progress notifications are not implemented.

## Configuration

`tools.mcp.enabled` is the master switch. Each entry in `tools.mcp_servers` selects exactly one of `command` (stdio) or `url` (Streamable HTTP); setting both or neither on an enabled server fails configuration validation.

```yaml
tools:
  mcp:
    enabled: true
  mcp_servers:
    filesystem:
      command: npx
      args: ["-y", "@modelcontextprotocol/server-filesystem", "/path/to/dir"]
      enabled: true
    remote:
      url: https://mcp.example.com/mcp
      headers:
        Authorization: "Bearer $MCP_TOKEN"
      enabled: true
```

MCP `env` and `headers` values support dollar-variable expansion; an unset referenced variable is an error. This is specific to MCP server settings: the general configuration loader does not expand environment placeholders in arbitrary YAML values.

The schema default for `execution.network_policy` is `deny`, but the packaged defaults set `allow`. Explicit `deny` skips HTTP MCP servers; remote connections require outbound network access.

## Authentication and trust

HTTP servers can set `auth: oauth` for OAuth 2.1 authorization code with PKCE, or supply authorization headers. OAuth is only valid with `url`. `trust_level` defaults to `untrusted`; such server tools are gated at execution risk or above, and server-provided read-only hints cannot lower that gate. Set `trusted` only for servers you control.

`tools_include` and `tools_exclude` filter exposed tool names. MCP tools also pass the agent's tool policy and approval checks.

## Resources and prompts

The built-in `mcp_resources` and `mcp_prompts` tools expose list/read and list/get operations respectively. Specify `server` when more than one server is connected. Responses are external data and should be treated as untrusted content.

For field-level options, see the [configuration reference](../reference/configuration.en.md).
