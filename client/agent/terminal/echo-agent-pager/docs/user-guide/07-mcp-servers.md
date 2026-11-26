# MCP Servers

MCP (Model Context Protocol) servers extend EchoAgent with external tool integrations. They let EchoAgent interact with any service that implements the MCP standard.

---

## What Are MCP Servers?

An MCP server is a process that exposes tools to EchoAgent over a standardized protocol. When you configure an MCP server, its tools become available to the model alongside EchoAgent's built-in tools. The model can discover and call these tools during a session.

For example, a GitHub MCP server might expose tools like `create_issue`, `list_pull_requests`, and `search_code`. A database server might expose `query`, `list_tables`, and `describe_schema`.

See the [MCP specification](https://modelcontextprotocol.io) for protocol details.

---

## Configuration

MCP servers are configured in `~/.echo-agent/config.toml` under `[mcp_servers.<name>]` sections.

To distribute MCP servers to a team, or to restrict which servers users may run, see [Distribute across an organization](09-plugins.md#distribute-across-an-organization) in the Plugins guide.

### stdio Transport (Local Process)

EchoAgent spawns a local process and communicates over stdin/stdout:

```toml
[mcp_servers.my-server]
command = "/path/to/server"           # Server executable
args = ["--flag", "value"]            # Command arguments
env = { API_KEY = "sk-..." }          # Environment variables
enabled = true                        # Enable or disable the server (default: true)
startup_timeout_sec = 30              # Server startup timeout, seconds (default: 30)
tool_timeout_sec = 6000               # Per-tool-call timeout fallback, seconds (default: 6000)
tool_timeouts = { slow_op = 120 }     # Per-tool timeout overrides, seconds
```

> **Global startup-timeout override:** instead of setting `startup_timeout_sec`
> per server, you can change the default for all servers via the `MCP_TIMEOUT`
> environment variable (milliseconds, compatible with Claude Code) or
> `ECHO_AGENT_MCP_STARTUP_TIMEOUT_SECS` (seconds). A per-server `startup_timeout_sec`
> still takes precedence over both. Cold-start `npx`/`uvx` servers that download
> packages on first launch often need this; the default is 30s.
>
> **MCP tool-result size cap:** large MCP / `use_tool` results are truncated
> inline (full payload spilled under the session `mcp/` folder). Default is
> **20_000 bytes**. Override via:
>
> - env `ECHO_AGENT_MAX_MCP_OUTPUT_BYTES` or `MAX_MCP_OUTPUT_BYTES` (bytes; EchoAgent-native
>   wins if both set; Claude-style name, but we bound by **bytes** not tokens)
> - `config.toml` — user-level (`~/.echo-agent/config.toml`) **or repo-level**
>   (`.echo-agent/config.toml` anywhere on the cwd → git-root chain; the deepest
>   file wins, and the repo value applies only once the folder is trusted):
>
> ```toml
> [mcp]
> max_output_bytes = 40000
> ```
>
> Precedence: requirements.toml > env > repo `.echo-agent/config.toml` >
> user/managed config > default. Repo edits apply to running sessions in that
> directory via config hot-reload.

### HTTP/SSE Transport (Remote Server)

For remote MCP servers accessible over HTTP:

```toml
[mcp_servers.remote-api]
url = "https://mcp.example.com/api"
headers = { "Authorization" = "Bearer token" }
```

MCP data-plane requests (JSON-RPC and SSE) and the anonymous-access probe carry a
default `User-Agent: echo-agent-cli/<version>` header, where `<version>` is the EchoAgent binary
version. OAuth discovery, client registration, and token requests are issued by the
rmcp OAuth client and keep its own behavior (no default `User-Agent`). A valid
`User-Agent` entry in the server's `headers` overrides the default; an invalid
configured `User-Agent` value is dropped by header parsing (with a warning), so such a
server still receives the default. Exception: Figma MCP servers (server name `figma`,
legacy managed name `echo_agent_com_figma`, or a `figma.com` host — all case-insensitive)
send the bare token `echo-agent-cli` with no version unless the config supplies its own
`User-Agent`.

### Streamable HTTP with Session ID

```toml
[mcp_servers.my-streamable-server]
url = "https://mcp.example.com/api/mcp"
headers = { "x-mcp-session-id" = "{{session_id}}" }
```

---

## CLI Management

Manage MCP servers from the command line without editing config files:

```bash
# List configured MCP servers
echoagent mcp list
echoagent mcp list --json          # Machine-readable output

# Add a stdio server. Everything after -- is the server command, so flags
# like -y reach the server instead of being parsed by echoagent.
echoagent mcp add filesystem -- npx -y @modelcontextprotocol/server-filesystem /path/to/dir

# Add a stdio server with environment variables (-e is repeatable)
echoagent mcp add postgres -e DATABASE_URL=postgres://localhost/mydb -- npx -y @modelcontextprotocol/server-postgres

# Add a remote HTTP server
echoagent mcp add --transport http sentry https://mcp.sentry.dev/mcp

# Add a remote server with an authentication header (--header is repeatable)
echoagent mcp add --transport http api https://mcp.example.com/mcp --header "Authorization: Bearer YOUR_TOKEN"

# Add a remote SSE server
echoagent mcp add --transport sse linear https://mcp.linear.app/sse

# Remove a server
echoagent mcp remove github

# Enable or disable a local/TOML (or compat-sourced) server
echoagent mcp enable github
echoagent mcp disable github

# Diagnose a server's configuration and connectivity
echoagent mcp doctor               # Check every configured server
echoagent mcp doctor github        # Check one server
echoagent mcp doctor --json        # Machine-readable output
```

The transport defaults to `stdio`; pass `--transport http` or `--transport sse` for remote servers.

By default `echoagent mcp add` writes to `~/.echo-agent/config.toml` (`--scope user`). Use `--scope project` to write to `.echo-agent/config.toml` in the current directory instead, which can be committed and shared with your team (see [Project-Scoped MCP Servers](#project-scoped-mcp-servers)). Header and environment variable values are stored verbatim, so reference secrets as `${VAR}` instead of pasting them into a committed project config (see [Example Configurations](#example-configurations)). `echoagent mcp list` shows servers from both scopes, marking project-scoped ones with `(project)` and disabled ones with `(disabled)`.

`echoagent mcp remove` searches both scopes and exits 0 after removing the server. It exits 1 when the name is not found, or when the name is defined in both user and project scope — pass `--scope` to say which one to remove.

`echoagent mcp enable` / `disable` persist the personal on/off state to user `~/.echo-agent/config.toml` (`disabled_mcp_servers`, and `[mcp_servers.<name>].enabled` when that entry exists). Scope:

- **Known names:** user/project EchoAgent TOML, names already on the disabled list, compat sources (`.mcp.json`, Claude, Cursor), and **plugin** MCP servers (same discovery as doctor/`/mcps`).
- **Enable only:** if the cwd-nearest project definition has sticky `enabled = false`, that single key is cleared (comments preserved); disable never rewrites project configs.
- **Not full `/mcps` parity:** gateway connectors (`managed_gateway:…`, stored under `disabled_mcp_tools.__managed_gateway_connectors`) stay Space-only in the TUI. Idempotent; unknown names exit 1.

Breaking changes from earlier releases: `--env` now takes one `KEY=value` per flag (use `-e A=1 -e B=2`, not `--env A=1 B=2`), and server names may only contain letters, numbers, hyphens, and underscores.

---

## Project-Scoped MCP Servers

MCP servers can be configured per-project by placing a `.echo-agent/config.toml` in your repository:

```
my-project/
  .echo-agent/
    config.toml
  src/
  ...
```

```toml
# .echo-agent/config.toml
[mcp_servers.linear]
url = "https://mcp.linear.app/mcp"
enabled = true
```

When a server exposes a native HTTP/SSE endpoint, prefer the `url` form over wrapping it in a stdio proxy such as `npx mcp-remote <url>`. EchoAgent handles HTTP/SSE and OAuth directly, so the native form avoids an extra subprocess per session. It also registers EchoAgent's own OAuth client with the provider.

EchoAgent walks from the current directory up to the git repo root, loading `.echo-agent/config.toml` at each level:

| Location | Scope | Priority |
|----------|-------|----------|
| `~/.echo-agent/config.toml` | All projects | Lowest |
| `<repo-root>/.echo-agent/config.toml` | This repository | Medium |
| `<cwd>/.echo-agent/config.toml` | Current directory | Highest |

If a project defines a server with the same name as a global one, the project version replaces it entirely (fields are not merged).

Project-scoped files contribute `[mcp_servers]`, `[plugins]`, and `[permission]` entries. EchoAgent reads most other config sections only from `~/.echo-agent/config.toml`.

---

## Tool Naming

MCP tools are namespaced with the server name to avoid collisions:

- Server `filesystem` with tool `read_file` becomes `filesystem__read_file`
- Server `github` with tool `create_issue` becomes `github__create_issue`

---

## Toggle Servers at Runtime

You can enable or disable MCP servers without restarting EchoAgent (TUI `/mcps` or CLI — see [CLI Management](#cli-management)).

### The /mcps Modal

Open the MCP servers modal in the TUI:

- Run `/mcps` as a slash command
- Or press `Ctrl+L` (non–VS Code family) and navigate to the MCP Servers tab; on VS Code family use `/plugins` or `/mcp` and open the MCP Servers tab

From the modal you can:

- See each server's source, enabled state, and tool count
- Enable or disable a server with `Space`
- Expand a server to view the tools it provides
- Refresh the list with `r` after you edit `config.toml`
- Authenticate an OAuth server with `i`
- Add a server with `a`, or remove a local server with `x` (the modal asks for confirmation; press lowercase `y` to remove, or any other key to cancel)

### Tool Discovery

The model has access to two built-in tools for working with MCP servers:

- `search_tool` — Discover available integration tools across all enabled MCP servers. Use this to find tools by name or description.
- `use_tool` — Call an integration tool discovered via `search_tool`. Specify the fully-qualified tool name (e.g., `github__create_issue`).

---

## Compatibility

EchoAgent loads MCP server configurations from multiple sources for compatibility:

| Source | Format | Location | Configurable |
|--------|--------|----------|-------------|
| `config.toml` | Native EchoAgent config | `~/.echo-agent/config.toml`, `.echo-agent/config.toml` | Always on |
| `.claude.json` | Claude Code format | `~/.claude.json` | `[compat.claude] mcps` |
| `.cursor/mcp.json` | Cursor format | `~/.cursor/mcp.json`, `<project>/.cursor/mcp.json` | `[compat.cursor] mcps` |
| `.mcp.json` | MCP standard format | Project root (cwd to git root) | Loaded unless you have imported or dismissed the Claude import prompt (the import marker is set) |

All sources are merged in priority order: config.toml > Claude > Cursor > `.mcp.json`. Servers from higher-priority sources take precedence when names conflict.

The Claude and Cursor MCP sources are scanned by default. To disable scanning for a specific vendor, set `[compat.<vendor>] mcps = false` in `~/.echo-agent/config.toml` or the corresponding environment variable (`ECHO_AGENT_CURSOR_MCPS_ENABLED`, `ECHO_AGENT_CLAUDE_MCPS_ENABLED`). See [Configuration](05-configuration.md#harness-compatibility) for details. Use `echoagent inspect` to see which MCP servers were loaded and their vendor origin (`[cursor]`, `[claude]`).

---

## MCP OAuth

For MCP servers that require OAuth authentication, EchoAgent handles the credential flow automatically. When an MCP server requests OAuth credentials, EchoAgent opens a browser-based authorization flow and stores the resulting tokens for future use.

---

## Example Configurations

Use the `url` form for hosted MCP servers and the `command` / `args` form for local stdio tools.

### Native HTTP (hosted services)

You must authenticate OAuth-based MCP servers before you can use them. EchoAgent stores the resulting tokens under `~/.echo-agent/mcp_credentials.json` as local plaintext with owner-only file permissions (`0600` on Unix). Prefer full-disk encryption on the host. After you edit `config.toml`, press `r` in the `/mcps` modal to refresh the server list.

```toml
[mcp_servers.linear]
url = "https://mcp.linear.app/mcp"
enabled = true

[mcp_servers.sentry]
url = "https://mcp.sentry.dev/mcp"
enabled = true

[mcp_servers.mixpanel]
url = "https://mcp.mixpanel.com/mcp"
enabled = true
```

For internal or self-hosted servers that authenticate with a static bearer token rather than OAuth, set the `Authorization` header explicitly:

```toml
[mcp_servers.internal-tools]
url = "https://mcp.internal.example.com/mcp"
enabled = true

[mcp_servers.internal-tools.headers]
Authorization = "Bearer <token>"
```

To avoid putting secrets in the config file, reference an environment variable with `${VAR}` (or `${VAR:-default}`). EchoAgent expands string fields in `[mcp_servers.*]` — `url`, `command`, `args`, and the values in `env` and `headers` — at load time:

```toml
[mcp_servers.internal-tools]
url = "https://mcp.internal.example.com/mcp"
enabled = true
headers = { "Authorization" = "Bearer ${INTERNAL_MCP_TOKEN}" }
```

### Local stdio

Use stdio for tools that must run locally (filesystem access, local databases, in-house servers).

```toml
# Filesystem access scoped to a directory
[mcp_servers.filesystem]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/path/to/allowed/directory"]

# Local Postgres
[mcp_servers.postgres]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-postgres", "postgresql://user:pass@localhost/db"]

# Custom server with a longer startup timeout and tuned per-tool timeouts
[mcp_servers.my-tools]
command = "/usr/local/bin/my-mcp-server"
args = ["--config", "/etc/my-mcp.json"]
startup_timeout_sec = 30
tool_timeout_sec = 120
tool_timeouts = { slow_analysis = 300, quick_lookup = 10 }
```

On Windows, npm installs launchers like `npx`, `npm`, `pnpm`, and `yarn` as `.cmd` batch shims (there is no `npx.exe`). EchoAgent resolves a bare `command` such as `npx` to its real launcher path on `PATH` (honoring `PATHEXT`) before spawning, so these work without manually wrapping them in `cmd /c`. A `command` given as an absolute path or one containing a path separator is used as-is.

---

## Available MCP Servers

A partial list of MCP servers you can configure with the `url` or `command` forms shown above. Confirm the current endpoint or package name with each provider before use:

| Server | Transport | Endpoint / Package |
|--------|-----------|--------------------|
| Linear | HTTP (OAuth) | `https://mcp.linear.app/mcp` |
| Sentry | HTTP (OAuth) | `https://mcp.sentry.dev/mcp` |
| Mixpanel | HTTP (OAuth) | `https://mcp.mixpanel.com/mcp` |
| Filesystem | stdio | `@modelcontextprotocol/server-filesystem` |
| Git | stdio | `@modelcontextprotocol/server-git` |
| GitHub | stdio | `@modelcontextprotocol/server-github` |
| GitLab | stdio | `@modelcontextprotocol/server-gitlab` |
| PostgreSQL | stdio | `@modelcontextprotocol/server-postgres` |
| SQLite | stdio | `@modelcontextprotocol/server-sqlite` |
| Puppeteer | stdio | `@modelcontextprotocol/server-puppeteer` |

See the [MCP Server Registry](https://github.com/modelcontextprotocol/servers) for the full list of community servers and the [MCP specification](https://modelcontextprotocol.io) for protocol details.

---

## Subagents and MCP

Subagents inherit the parent session’s connected MCP servers by default, including plugin-sourced agents. Use agent frontmatter `mcpInheritance` to restrict that set (`all`, `none`, `named`, or `except`). Details are in [Subagents — MCP inheritance](16-subagents.md#mcp-inheritance).

If a child lists `search_tool` / `use_tool` but returns an empty catalog, check that:

1. The parent session actually connected the server (see Extensions / `echoagent inspect`)
2. The agent’s `mcpInheritance` is not `none` or a filter that excludes the server
3. Plugin agents cannot declare their own `mcpServers` in frontmatter — they only see parent-connected servers

---

## Troubleshooting

### Server Not Starting

```bash
# Test the server command manually
npx -y @modelcontextprotocol/server-filesystem /path

# Increase startup timeout
# In config.toml:
[mcp_servers.filesystem]
startup_timeout_sec = 30
```

For stdio servers, EchoAgent captures the process's standard error to `~/.echo-agent/logs/mcp/<server>.stderr.log`, truncated on each launch. Check this file when a server starts but fails to handshake:

```bash
tail -f ~/.echo-agent/logs/mcp/filesystem.stderr.log
```

### Viewing Server Status

Use `echoagent inspect` to see all loaded MCP servers and their sources:

```bash
echoagent inspect          # Human-readable
echoagent inspect --json   # Machine-readable
```

### Debug Logging

```bash
RUST_LOG=debug ECHO_AGENT_LOG_FILE=/tmp/echoagent.log echoagent
tail -f /tmp/echoagent.log
```

Look for log entries containing `mcp` to trace server startup, tool discovery, and tool call execution.
