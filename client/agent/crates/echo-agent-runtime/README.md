# EchoAgent Runtime

> **Embedded integration:** this source is maintained for EchoAgent Desktop.
> EchoAgent Desktop supplies BYOK provider URLs and credentials. Addresses
> under `echo-agent.invalid` are deliberate fail-closed placeholders for
> hosted services that this repository does not operate. Use the desktop app's
> provider settings for normal model access.

A terminal-based AI coding assistant and agentic harness.

Use it interactively as a TUI, or integrate it into your own apps via headless mode and the Agent Client Protocol (ACP).

## Quick Start

```bash
# Build the maintained runtime workspace
cargo build --locked -p echo-agent-pager-bin

# Interactive TUI
cargo run --locked -p echo-agent-pager-bin

# Headless (for scripts/automation)
echoagent -p "Explain this codebase"

# Agent mode (for IDE/app integration)
echoagent agent stdio
```

## Contents

- [Installation](#installation)
- [Authentication](#authentication) — browser login, API key, OIDC, external auth providers
- **Using EchoAgent**
  - [Interactive TUI](#interactive-tui) — shortcuts, slash commands, file references
  - [Headless Mode](#headless-mode) — scripting, CI/CD, output formats
  - [Agent Mode](#agent-mode) — stdio, ACP integration
  - [SSH Passthrough](#ssh-passthrough-echo-agent-ssh) — Apple Terminal clipboard support
- **Configuration**
  - [Config File](#configuration) — general settings, telemetry, LSP, enterprise deployment
  - [Custom Models](#custom-models) — BYOK, Ollama, OpenAI, custom endpoints
  - [MCP Servers](#mcp-servers) — external tool integrations
- **Customization**
  - [Project Rules (AGENTS.md)](#agentsmd) — per-project system prompt instructions
  - [Skills](#skills) — reusable prompt packages
  - [Agent Profiles](#agent-profiles) — custom agent definitions
  - [Subagents](#subagents) — parallel child sessions, roles, personas
  - [Plugins](#plugins) — external tool/skill packages
  - [Hooks](#hooks) — project lifecycle scripts
- **Features**
  - [Memory](#memory) — cross-session knowledge persistence
  - [Sandbox](#sandbox) — OS-level filesystem/network isolation
- **Reference**
  - [Introspection (`echoagent inspect`)](#introspection)
  - [Claude Code Compatibility](#claude-code-compatibility)
  - [Built-in Tools](#built-in-tools)
  - [Session Persistence](#session-persistence) — storage layout, resume
  - [File Locations](#file-locations)
  - [Environment Variables](#environment-variables)
  - [Troubleshooting](#troubleshooting)
- [Building with EchoAgent](#building-with-echo-agent) — headless API, ACP SDK integration

---

## Installation

```bash
# Install latest stable
curl -fsSL https://echo-agent.invalid/cli/install.sh | bash

# Install a specific version
curl -fsSL https://echo-agent.invalid/cli/install.sh | bash -s 0.1.42
```

Verify installation:

```bash
echoagent --version
```

Update to the latest version:

```bash
echoagent update
```

---

## Authentication

### Browser Login (Default)

On first launch, EchoAgent opens your browser to authenticate with cloud.echo-agent.invalid:

```bash
echoagent
```

Credentials are stored in `~/.echo-agent/auth.json` and persist across sessions. Tokens expire after 7 days; EchoAgent will prompt you to re-authenticate when needed.

### Re-authenticate

To switch accounts or fix authentication issues:

```bash
echoagent login
```

### API Key

For CI/CD, automation, or environments without browser access, use an API key from [console.echo-agent.invalid](https://console.echo-agent.invalid):

```bash
export ECHO_AGENT_API_KEY="echo-agent-..."
echoagent
```

The API key takes precedence over browser credentials.

### OIDC (Customer SSO)

Authenticate developers via your own Identity Provider (Okta, Azure AD, Auth0) instead of `accounts.echo-agent.invalid`.

**1. Register a public client in your IdP:**
- Grant type: Authorization Code with PKCE
- Redirect URI: `http://127.0.0.1/callback` (the CLI uses a random ephemeral port; most IdPs treat loopback redirects as port-agnostic per [RFC 8252 §7.3](https://tools.ietf.org/html/rfc8252#section-7.3))
- No client secret (PKCE only, per [RFC 8252](https://tools.ietf.org/html/rfc8252))

**2. Configure the CLI** (config file or env vars):

```toml
# ~/.echo-agent/config.toml
[echo_agent_com_config.oidc]
issuer = "https://acme.okta.com"
client_id = "0oa1b2c3d4e5f6g7h8i9"
```

```bash
# Or via environment variables
export ECHO_AGENT_OIDC_ISSUER="https://acme.okta.com"
export ECHO_AGENT_OIDC_CLIENT_ID="0oa1b2c3d4e5f6g7h8i9"
```

Customers typically also override the API endpoint to point at their own proxy:
```bash
export ECHO_AGENT_CLI_CHAT_PROXY_BASE_URL="https://echo-agent-proxy.acme.com/v1"
```

**3. Run `echoagent`.** The CLI discovers endpoints via `{issuer}/.well-known/openid-configuration`, opens the IdP login page, and stores tokens in `~/.echo-agent/auth.json`. The OIDC token is sent as `Authorization: Bearer` to the configured proxy. Tokens auto-refresh silently via the stored `refresh_token`.

**Optional fields:**

| Field | Default | Notes |
|-------|---------|-------|
| `scopes` | `["openid", "profile", "email", "offline_access"]` | `offline_access` enables silent token refresh; add custom scopes if needed |
| `audience` | None | Required by some IdPs (e.g. Auth0) |

### External Auth Provider

For environments where browser-based login isn't possible (sandboxed VMs, CI runners, air-gapped networks), delegate authentication to an external binary or script. This is the recommended approach for enterprise deployments where your company runs its own auth infrastructure (SSO, device code flows, certificate auth, etc.).

EchoAgent is provider-agnostic — it doesn't know or care how your binary authenticates. It just runs the command, reads a token from stdout, and stores it. Your binary is a black box that handles the entire auth flow.

#### How It Works

```
┌──────────────┐     sh -c     ┌────────────────────────┐
│     EchoAgent     │──────────────▶│  your auth binary      │
│              │               │                        │
│  reads       │◀── stdout ────│  prints token          │
│  auth.json   │               │                        │
│              │   (stderr)    │  prints status/URLs    │──▶ user's terminal
└──────────────┘               └────────────────────────┘
```

1. EchoAgent runs your command via `sh -c "<command>"`
2. Your binary does whatever auth flow it needs (SSO login, device code, cert exchange, etc.)
3. **stderr** → displayed directly to the user (use for login URLs, status messages, progress)
4. **stdout** → captured by EchoAgent and saved to `~/.echo-agent/auth.json` as the access token
5. exit 0 → success; exit non-zero → EchoAgent falls through to interactive login

#### The stdout / stderr Contract

This is the most important thing to get right:

| Stream | What to print | Who sees it |
|--------|---------------|-------------|
| **stdout** | The token — nothing else | EchoAgent (parsed and stored in `auth.json`) |
| **stderr** | Login URLs, status messages, errors, progress | The user (displayed in their terminal) |

**Do not print anything to stdout except the token.** No progress messages, no debug output, no "Login successful!" text. EchoAgent reads stdout verbatim and tries to parse it as a token. Any extra text will break parsing.

#### stdout Token Format

The token on stdout can be either:

**1. Bare string** — just the raw token, nothing else:
```
eyJhbGciOiJSUzI1NiIs...
```

**2. JSON** — with optional refresh token and expiry:
```json
{"access_token": "eyJhbGciOi...", "refresh_token": "ref-tok", "expires_in": 3600}
```

Use JSON if your tokens expire and you want EchoAgent to automatically re-run the binary before expiry. The `expires_in` field (seconds until expiry) tells EchoAgent when to proactively refresh. Without it, EchoAgent assumes tokens last 30 days.

#### Minimal Example

```bash
#!/bin/sh
# Print login URL / status to stderr (user sees this)
echo "Authenticating via Acme Corp SSO..." >&2
echo "Visit: https://sso.acme.com/device-login?code=ABCD-1234" >&2

# ... do the auth flow, get a token ...

# Print ONLY the token to stdout (EchoAgent captures this)
echo "eyJhbGciOiJSUzI1NiIs..."
```

#### Configuration

```toml
# ~/.echo-agent/config.toml
[auth]
auth_provider_command = "/usr/local/bin/my-auth-provider"
auth_provider_label = "Acme Corp"   # optional — customizes the TUI login button
auth_token_ttl = 3600               # optional — token lifetime in seconds (see below)
```

```bash
# Or via environment variables
export ECHO_AGENT_AUTH_PROVIDER_COMMAND="/usr/local/bin/my-auth-provider"
export ECHO_AGENT_AUTH_PROVIDER_LABEL="Acme Corp"   # optional
export ECHO_AGENT_AUTH_TOKEN_TTL=3600               # optional
```

If your binary outputs a bare token string (not JSON with `expires_in`), set `auth_token_ttl` to the token's expected lifetime in seconds. Without it, EchoAgent cannot detect expiry proactively and will only refresh after a 401.

The command runs through the platform shell — `sh -c` on macOS/Linux, `cmd /C` on Windows — so it can be a binary path, a script, or a pipeline.

> **Windows:** write the path as a TOML *literal* string (single quotes) so backslashes survive: `auth_provider_command = 'C:\corp\echo-agent-auth.exe'`. Inside a double-quoted TOML string `\t`, `\n`, `\r`, `\b` and `\f` are escape sequences, so `"C:\temp\auth.exe"` parses into a path containing a tab character and the provider fails to start — after which EchoAgent falls back to browser login as if the setting were ignored.

When `auth_provider_label` is set, the TUI welcome screen shows **"Login with Acme Corp"** instead of "Login with cloud.echo-agent.invalid". In headless mode (`echoagent -p`), the label has no effect — stderr from your binary is printed directly to the terminal.

> **Enterprise setup:** For a complete enterprise `config.toml` combining external auth, corporate proxy, and telemetry settings, see [Enterprise Deployment](#enterprise-deployment) in the Configuration section.

#### Example: Device Code Flow Provider

```bash
#!/bin/sh
# 1. Request device code from your IdP
RESP=$(curl -s -X POST https://auth.acme.com/device/code -d "client_id=echo-agent-cli")
CODE=$(echo "$RESP" | jq -r '.user_code')
URL=$(echo "$RESP" | jq -r '.verification_uri')
DEVICE_CODE=$(echo "$RESP" | jq -r '.device_code')

# 2. Show login URL to user (stderr — user sees this in their terminal)
echo "Open $URL and enter code: $CODE" >&2

# 3. Poll until user approves
while true; do
  TOKEN=$(curl -s -X POST https://auth.acme.com/device/token \
    -d "device_code=$DEVICE_CODE&grant_type=urn:ietf:params:oauth:grant-type:device_code" \
    | jq -r 'select(.access_token) | .access_token')
  [ -n "$TOKEN" ] && break
  sleep 5
done

# 4. Print token to stdout — JSON format enables auto-refresh
echo "{\"access_token\": \"$TOKEN\", \"expires_in\": 3600}"
```

#### Example: Auth Binary with Refresh Support

EchoAgent runs your binary on two different contracts, and `ECHO_AGENT_AUTH_EXPIRED` is how it tells them apart:

| | `ECHO_AGENT_AUTH_EXPIRED=1` | unset |
|---|---|---|
| **What it is** | A headless refresh over a credential EchoAgent already holds — near-expiry rotation, or a token the server rejected | A sign-in: `echoagent login`, the sign-in screen, or the escalation after a headless run couldn't mint |
| **Is anyone watching?** | No. stdin is closed and nothing renders your prompts | Yes. A user is waiting, and your stderr reaches them |
| **Budget** | A few seconds (7s), then EchoAgent kills the process | 300s — enough for a browser round trip or a device code |
| **So your binary should** | Mint silently, or **exit non-zero**. Never block | Do the full SSO flow, and always mint fresh |

```bash
#!/bin/sh
if [ "$ECHO_AGENT_AUTH_EXPIRED" = "1" ]; then
    # Headless: silent refresh only. If that can't work — the SSO session
    # lapsed, say — exit non-zero rather than block. EchoAgent then shows the
    # sign-in screen, which re-runs this binary with the variable unset.
    echo "Refreshing token..." >&2
    TOKEN=$(my-company-auth --refresh --silent) || exit 1
else
    # A user is attached — full interactive SSO flow.
    echo "Authenticating via Acme Corp SSO..." >&2
    TOKEN=$(my-company-auth --login --interactive)
fi

if [ -z "$TOKEN" ]; then
    echo "Authentication failed" >&2
    exit 1
fi

echo "{\"access_token\": \"$TOKEN\", \"expires_in\": 3600}"
```

Exiting promptly on `ECHO_AGENT_AUTH_EXPIRED=1` is what makes the handover to the sign-in screen fast: a binary that blocks instead pays the whole refresh timeout on every start with an expired token.

One case stays ambiguous, and only in **leader mode** (`--leader`, or `[cli] use_leader = true`; off by default): with no credential at all, the leader makes one extra attempt in the background just after startup, and that run has the variable unset, like a sign-in. A binary that mints without help (service account, keytab, mounted token) succeeds there and the session heals itself. One that must prompt just sits, up to the 300s sign-in ceiling — nothing waits on it, the sign-in screen is already up, and its stderr goes to `~/.echo-agent/leader.log` rather than to the user.

`ECHO_AGENT_AUTH_EXPIRED` is optional — if your binary ignores it, EchoAgent still works. It just runs the same flow for both login and refresh, and a flow that prompts will be killed on the headless run before it can finish.

### Automatic Credential Refresh

EchoAgent supports automatic credential refresh for external auth providers and OIDC. When EchoAgent detects that your token is expired (either locally based on `expires_in`, or when the server returns a 401), it automatically re-runs your `auth_provider_command` to obtain new credentials before retrying the request.

This is transparent — you don't need to do anything. EchoAgent handles it in the background during your session.

**When does refresh happen?**

- **Before expiry:** If your binary returned `expires_in` in its JSON output, or you set `auth_token_ttl` in config, EchoAgent re-runs the binary ~5 minutes before the token expires, so you never see an auth error.
- **On auth error:** If the server rejects a request with 401/403 (e.g. token was revoked or expired), EchoAgent re-runs the binary and retries the request once.
- **When the refresh run can't mint:** refreshes are headless (no stdin, short timeout), so a binary that needs you to complete an SSO flow cannot succeed there. EchoAgent then stops treating the stored credential as usable and runs your binary in its interactive mode instead — at startup that is the same sign-in flow a machine with no credentials gets; mid-session the turn fails with a re-auth prompt and `/login` re-runs the binary.
- **OIDC:** If you're using OIDC and have a `refresh_token`, EchoAgent silently refreshes via your IdP without re-opening the browser.

**Tuning the refresh buffer:**

```bash
# EchoAgent refreshes tokens 5 minutes before expiry by default.
# Set to 0 to only refresh on 401. Set higher for very short-lived tokens.
export ECHO_AGENT_AUTH_EARLY_INVALIDATION_SECS=300
```

**Keep in mind:**
- When using `auth_provider_command`, you don't need to run `echoagent login` before starting — EchoAgent runs your binary automatically on first launch. You _can_ run `echoagent login` to explicitly hydrate `auth.json` ahead of time if you prefer.
- If both OIDC and `auth_provider_command` are configured: at **login** time, EchoAgent tries OIDC silent refresh first (if a `refresh_token` exists), then the external binary, then browser-based login. During a **session**, whichever method is configured is used exclusively — if `auth_provider_command` is set it handles all mid-session refreshes; otherwise OIDC silent refresh is used.
- Your binary's stderr output is displayed to the user but interactive stdin is not supported. This works well for browser-based SSO flows where the binary displays a URL and you complete authentication in the browser.

#### Troubleshooting Auth

Enable debug logging to trace the auth flow:

```bash
echoagent --debug-file /tmp/echo-agent-auth.log -p "hello"
tail -f /tmp/echo-agent-auth.log
```

Common log messages:

| Log message | What it means |
|-------------|---------------|
| `auth: running external auth provider (headless refresh)` | Your binary is being called with `ECHO_AGENT_AUTH_EXPIRED=1` and a few seconds to work |
| `auth: running external auth provider (interactive login)` | Your binary is being called on the sign-in contract: no `ECHO_AGENT_AUTH_EXPIRED`, stderr shown, 300s |
| `auth: external auth provider returned fresh token` | Success — token was parsed and stored |
| `auth: external auth provider failed` | Binary exited non-zero, or exited 0 but stdout was empty/unparseable (the `error` field has details) |
| `auth: external auth provider timed out (likely needs interactive auth), killing` | Binary didn't exit before the 7s headless-refresh timeout and was killed. Exiting non-zero on `ECHO_AGENT_AUTH_EXPIRED=1` avoids this wait entirely |
| `auth: failed to start external auth provider` | The command couldn't be spawned (e.g. binary not found) |

### Per-Model Auth Providers

`auth_provider_command` above replaces EchoAgent's *session* auth: it mints the token sent to EchoAgent's backend. If you instead want EchoAgent models on normal EchoAgent login while **other models** route through a gateway (LiteLLM, corporate proxy) whose bearer tokens rotate, use a named auth provider — the rotating-token analogue of a per-model `api_key`/`env_key`.

```toml
# ~/.echo-agent/config.toml
[auth_provider.litellm]
command = "/usr/local/bin/litellm-token"   # run via `sh -c`
token_ttl_secs = 3600                      # optional: see below
timeout_secs = 10                          # optional: command timeout (default 30)

[model.proxied-claude]
model = "claude-sonnet-4-5"
base_url = "https://litellm.corp.example/v1"
context_window = 200000
auth_provider = "litellm"
```

**Contract** (same stdout contract as `auth_provider_command`; the `issuer` field is accepted but unused here, and `refresh_token`, when present, is handed back to the command on refresh):

- Without `args`, the command runs via POSIX `sh -c`, so it can be a binary path, a script, or a pipeline. With `args = ["..."]`, the command runs directly with those arguments and no shell: `command` is a program name resolved via `PATH`, or a path. Use `args` to avoid shell quoting, and on Windows, where there is no `sh`.
- stdout: a bare token, or JSON `{"access_token": "...", "expires_in": 3600}`.
- stderr: logged when the command fails; exit 0 = success.
- `ECHO_AGENT_AUTH_EXPIRED=1` is set whenever EchoAgent re-mints over a token still cached in memory, whether from near-expiry rotation or a rejection. The first mint on a cold cache runs without it.

**Token lifecycle:**

- Tokens are cached in memory per provider and shared by every model referencing the provider; nothing is written to disk. The command is a credential helper: it owns durable storage and OAuth2 refresh (keychain, its own dotdir, etc.), exactly like `gcloud auth print-access-token` or a git credential helper. On an in-session re-mint the last credential is handed back via `ECHO_AGENT_AUTH_PROVIDER_ACCESS_TOKEN` (and, when present, `ECHO_AGENT_AUTH_PROVIDER_REFRESH_TOKEN` / `ECHO_AGENT_AUTH_PROVIDER_EXPIRES_AT`), so a refresh-grant command can refresh instead of re-authenticating. The command must be non-interactive and fast; do any interactive login out of band, and EchoAgent re-runs the command on restart to re-mint.
- EchoAgent runs the command before a chat turn when the token is missing or within about a minute of expiring, and once more after the server rejects a token. A token rejected within 30 seconds of being fetched is not refetched again, so a broken helper surfaces one clear error instead of looping.
- Token lifetime comes from `expires_in` in the command's JSON output, else `token_ttl_secs`, else the token's own JWT expiry claim. With none of these, tokens are only replaced after the server rejects one.
- Commands run with a `timeout_secs` bound (default 30, clamped to 1..=600) and are killed on timeout. A turn waits on the run, so keep helpers fast and non-interactive.
- Active sessions pick up edits or removal of a provider table at the next model switch or new session. Once picked up, an edit invalidates the cached token, so the edited command runs at the next use; removal drops the cached token.
- Helper models (web search, session summary, image description) read the shared cache and never run the command; point them at providers your chat model keeps warm. Subagents refresh tokens the same way their parent session does.

**Interaction with other credentials:** a literal `api_key`/`env_key` on the model wins over its `auth_provider`. Provider-backed models are BYOK: your EchoAgent session token is never sent to their endpoints, and a failing provider command fails the request rather than falling back to the session token.

**Security:** provider commands execute code, so they are honored only from trusted config layers (`~/.echo-agent/config.toml`, managed config, requirements). A project's `.echo-agent/config.toml` can never define one. Whatever layer sets a model's `base_url` decides where that model's minted token is sent, and `base_url` (unlike the provider table) is not stripped from remote or campaign patches, the same as for a static `env_key`. Keep provider tables and the model `base_url` in layers you trust. The command inherits EchoAgent's environment (so it sees `PATH`, `HOME`, and any other secrets there), but EchoAgent's own first-party credentials (`ECHO_AGENT_API_KEY`, `ECHO_AGENT_DEPLOYMENT_KEY`, and related keys) are removed so a BYOK helper never receives them; write helpers that read only what they need, and prefer the `ECHO_AGENT_AUTH_PROVIDER_*` handback for the prior credential.

### Using auth.json for API Access

If you've authenticated with `echoagent login`, you can use the stored credentials to call the CLI chat proxy directly via curl. The proxy requires specific headers that mirror what the echoagent CLI sends internally:

```bash
curl -s -N -X POST "https://chat-proxy.echo-agent.invalid/v1/chat/completions" \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer $(jq -r '."https://accounts.echo-agent.invalid/sign-in".key' ~/.echo-agent/auth.json)" \
  -H "X-Echo-Agent-Token-Auth: echo-agent-cli" \
  -H "x-echo-agent-model-override: echo-agent-build" \
  -d '{
    "model": "echo-agent-build",
    "messages": [{"role": "user", "content": "Hello!"}],
    "stream": true
  }'
```

**Required headers:**

| Header                           | Required | Purpose                                                                                                                                                                                   |
| -------------------------------- | -------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `Authorization: Bearer <token>`  | Yes      | Session token from `~/.echo-agent/auth.json` (set by `echoagent login`)                                                                                                                              |
| `X-Echo-Agent-Token-Auth: echo-agent-cli` | Yes      | Tells the auth middleware to validate as a CLI session token                                                                                                                              |
| `x-echo-agent-model-override: <model>` | Yes\*    | The proxy uses this header (not the JSON body) to route to the correct backend. \*Can be omitted for `echo-agent-build` which is on the default route, but always safe to include. |

**Streaming vs non-streaming:**

Most models behind the proxy only support streaming. Always use `"stream": true` unless you know the model supports non-streaming.

| Model                 | Non-streaming  | Streaming    |
| --------------------- | -------------- | ------------ |
| `echo-agent-build`    | ✅ Supported   | ✅ Supported |

> **Note:** `auth.json` tokens expire after 7 days. Run `echoagent login` to refresh.

---

## Interactive TUI

The TUI (Terminal User Interface) provides a full interactive coding environment.

### Launch

```bash
echoagent [OPTIONS]
```

### Options

| Flag                       | Description                                                            |
| -------------------------- | ---------------------------------------------------------------------- |
| `--cwd <PATH>`             | Set working directory (default: current directory)                     |
| `--prompt <TEXT>`          | Send an initial prompt immediately after startup                       |
| `--rules <TEXT>`           | Append custom rules to the system prompt                               |
| `--always-approve`         | Auto-approve all tool executions without confirmation                  |
| `--sandbox <PROFILE>`      | OS-level filesystem/network guardrails (see [Sandbox](#sandbox))       |
| `--light`                  | Use light theme (macOS Basic) instead of dark                          |
| `--single-turn`            | Exit after first response (requires `--prompt`)                        |
| `--subagents`              | Enable subagent/task tool support (see [Subagents](#subagents))        |
| `--disable-web-search`     | Remove web search tool from the agent toolset                          |
| `--agent-profile <PATH>`   | Load a custom agent definition file (see [Agent Profiles](#agent-profiles)) |
| `--allow <RULE>`           | Permission allow rule with glob patterns (repeatable). See [Permission Rules](#permission-rules-allow--deny). |
| `--deny <RULE>`            | Permission deny rule with glob patterns (repeatable). See [Permission Rules](#permission-rules-allow--deny). |

### Examples

```bash
# Start in a specific project
echoagent --cwd ~/projects/my-app

# Start with an initial task
echoagent --prompt "Review this codebase and suggest improvements"

# Add project-specific rules
echoagent --rules "Always use TypeScript. Prefer functional components."

# Auto-approve mode for trusted tasks
echoagent --always-approve --prompt "Format all files"
```

### Keyboard Shortcuts

| Key                          | Action                          |
| ---------------------------- | ------------------------------- |
| `Enter`                      | Send message                    |
| `Shift+Enter` or `Alt+Enter` | Insert newline                  |
| `Ctrl+M`                     | Toggle multiline input mode     |
| `Ctrl+C` or `Esc`            | Cancel current operation        |
| `Ctrl+D` or `Ctrl+Q`         | Quit (with confirmation)        |
| `Ctrl+O`                     | Toggle always-approve mode |
| `Ctrl+T`                     | Toggle TODO/task panel          |
| `Ctrl+R`                     | Search prompt history           |
| `Ctrl+V`                     | Paste from clipboard            |
| `Ctrl+U`                     | Undo last input change          |
| `Ctrl+G`                     | Move foreground task to background |
| `Ctrl+P`                     | Toggle debug panel              |

### Slash Commands

Type `/` in the input to access commands:

| Command                            | Alias     | Description                                              |
| ---------------------------------- | --------- | -------------------------------------------------------- |
| `/model <name>`                    | `/m`      | Switch to a different model                              |
| `/new`                             |           | Start a new session (clears context)                     |
| `/load [workspace] [session]`      | `/resume` | Load a previous session                                  |
| `/rewind <prompt>`                 |           | Rewind to a previous prompt (restores files)             |
| `/compact [context]`               |           | Compact conversation history                             |
| `/always-approve [on\|off]`        | `/yolo`   | Toggle auto-approve mode                                 |
| `/multiline`                       | `/ml`     | Toggle multiline input mode                              |
| `/memory [workspace\|global] <text>` |         | Append text to a memory file (requires memory enabled) |
| `/flush`                           |           | Save current session knowledge to memory now             |
| `/skills [name]`                   |           | List skills or inject a skill into context               |
| `/plugins [list\|reload\|trust]`   | `/plugin` | Manage plugins (list, reload, trust)                     |
| `/hooks-list`                      |           | Show hooks loaded in this session                        |
| `/hooks-trust`                     |           | Trust this folder for hooks (writes folder trust)        |
| `/hooks-add <path>`                |           | Add a custom hook file or directory                      |
| `/feedback [message]`              |           | Report an issue or send feedback                         |
| `/exit`                            | `/quit`   | Exit the TUI                                             |

```bash
# Example usage in TUI:
/model echo-agent-build
/new
/rewind
/feedback Something isn't working
```

### Features

- **Syntax highlighting** for code blocks
- **Inline diffs** showing file changes before they're applied
- **Tool execution progress** with real-time output
- **TODO panel** tracking task progress
- **Session persistence** — conversations auto-save and can be resumed
- **History search** — `Ctrl+R` to search previous prompts

### File References (`@`)

Use the `@` operator in your prompt to attach file contents to your message. Type `@` followed by a filename or path to open a fuzzy file picker, then press `Tab` or `Enter` to select.

```
@src/main.rs              # Attach a file
@src/main.rs:10-50        # Attach lines 10–50 of a file
@src/                     # Browse a directory (end with /)
```

**Exposing hidden files with `!`**

By default, the `@` file picker respects `.gitignore` rules and hides dotfiles (files and directories starting with `.`). To search hidden files — such as `.github/`, `.vscode/`, `.env`, or other dotfiles — prefix your query with `!`:

```
@!.github                 # Search for .github/ and other hidden files
@!.vscode/settings.json   # Find .vscode/settings.json
@!.env                    # Attach a .env file
```

The `!` modifier allows you to attach any file in the project regardless of ignore rules.

---

## Headless Mode

Run EchoAgent non-interactively from the command line. Use headless mode when you need to:

- **Automate tasks** — CI/CD pipelines, pre-commit hooks, cron jobs
- **Script workflows** — Batch process files, chain with other tools
- **Build integrations** — Spawn as a sub-agent, embed in larger systems
- **Parse output programmatically** — JSON output for downstream processing

Headless mode accepts a single prompt, executes it with full tool access, and returns the result.

### Basic Usage

```bash
echoagent -p "Your prompt here"
```

### Options

| Flag                    | Description                                           |
| ----------------------- | ----------------------------------------------------- |
| `-p, --single <PROMPT>` | The prompt to send (required)                         |
| `-m, --model <MODEL>`   | Model to use (e.g., `echo-agent-build`)               |
| `-s, --session-id <ID>` | Create or resume a headless session with this ID      |
| `-r, --resume <ID_OR_TITLE>` | Resume an existing session by ID, or by title for the current directory, ignoring letter case (a sole explicitly renamed title wins among duplicates; remaining duplicates error with their IDs; UUID-shaped values are always treated as IDs) |
| `-c, --continue`        | Continue the most recent session in current directory |
| `--cwd <PATH>`          | Working directory                                     |
| `--output-format <FMT>` | Output format: `plain`, `json`, `streaming-json`      |
| `--always-approve`      | Auto-approve tool executions                          |
| `--rules <TEXT>`        | Custom rules for the system prompt                    |
| `--tools <TOOLS>`       | Allowlist of built-in tools (comma-separated). Only the listed tools will be available; all others are removed. Headless mode only. |
| `--disallowed-tools <TOOLS>` | Denylist of built-in tools to remove (comma-separated). Listed tools are stripped from the agent's toolset. Supports `Agent` / `Agent(type)` entries to restrict subagent spawning (see below). Headless mode only. |
| `--max-turns <N>`       | Maximum number of agentic turns before stopping       |
| `--reasoning-effort` / `--effort <LEVEL>` | Reasoning effort (`none`, `minimal`, `low`, `medium`, `high`, `xhigh`, `max`; also per-model menu ids like `deep`). TUI and headless. |
| `--permission-mode <MODE>` | Permission mode for tool approvals                 |
| `--allow <RULE>`        | Permission allow rule with glob patterns (repeatable). See below. |
| `--deny <RULE>`         | Permission deny rule with glob patterns (repeatable). See below.  |

#### Tool Filtering (`--tools` / `--disallowed-tools`)

Use `--tools` to restrict the agent to an explicit set of tools (allowlist), or `--disallowed-tools` to remove specific tools from the default set (denylist). Both accept a comma-separated list of tool names.

Tool names correspond to the internal tool IDs shown below. For quick reference:

| Display Name   | Tool ID for `--tools` / `--disallowed-tools` |
| -------------- | --------------------------------------------- |
| bash           | `run_terminal_cmd`                            |
| grep           | `grep`                                        |
| read_file      | `read_file`                                   |
| search_replace | `search_replace`                              |
| list_dir       | `list_dir`                                    |
| web_search     | `web_search`                                  |
| web_fetch      | `web_fetch`                                   |
| todo_write     | `todo_write`                                  |
| task           | `task`                                        |

```bash
# Only allow read-only tools
echoagent -p "Explain this codebase" --tools "read_file,grep,list_dir"

# Remove web access and file editing
echoagent -p "Review this code" --disallowed-tools "web_search,web_fetch,search_replace"

# Remove shell access
echoagent -p "Review this code" --disallowed-tools "run_terminal_cmd"
```

`--disallowed-tools` also supports special `Agent` entries to control subagent spawning:

| Entry                          | Effect                                                  |
| ------------------------------ | ------------------------------------------------------- |
| `Agent`                        | Block **all** subagent spawning                         |
| `Agent(explore)`               | Block the `explore` subagent type only                  |
| `Agent(explore, plan)`         | Block multiple specific types                           |

```bash
# Allow tools but prevent the agent from spawning any subagents
echoagent -p "Fix this bug" --disallowed-tools "Agent"

# Block only the explore subagent
echoagent -p "Refactor this module" --disallowed-tools "Agent(explore)"
```

When `--tools` is set, only the listed tools are available and default tool injection is disabled. When both flags are present, `--disallowed-tools` runs after `--tools` — use this to start from an allowlist and then remove specific entries.

> **Note:** `--tools`, `--disallowed-tools`, and `--max-turns` are only supported in headless mode (`-p`). If used in the interactive TUI, a warning is printed and the flag is ignored. `--reasoning-effort`/`--effort` and `--permission-mode` work in both modes.

#### Permission Rules (`--allow` / `--deny`)

Permission rules control whether specific tool invocations are auto-approved, denied, or require confirmation. Unlike `--disallowed-tools` (which removes tools entirely from the agent's toolset), permission rules leave tools available but gate their execution.

Rules use `ToolPrefix(glob_pattern)` syntax. Supported prefixes:

| Prefix        | What it controls                   |
| ------------- | ---------------------------------- |
| `Bash(...)`   | Shell command execution            |
| `Edit(...)`   | File editing (path glob)           |
| `Write(...)`  | File writing (path glob)           |
| `Read(...)`   | File reading (path glob)           |
| `Grep(...)`   | Search operations (path glob)      |
| `WebFetch(...)` | URL fetching (glob or `domain:host`) |
| `MCPTool(...)` | MCP tool invocations              |

Glob patterns support `*` (single-level wildcard) and `**` (recursive). A bare prefix without parentheses matches all invocations of that type. Claude Code's `Bash(cmd:*)` rules are also accepted and are equivalent to prefix matching on `cmd`.

```bash
# Deny all shell commands matching "rm*"
echoagent -p "Clean up this project" --deny "Bash(rm*)"

# Allow npm commands, deny everything else dangerous
echoagent -p "Set up the project" --allow "Bash(npm*)" --deny "Bash(sudo*)"

# Deny edits outside src/
echoagent -p "Refactor the code" --deny "Edit(/etc/**)"

# Allow all bash commands (auto-approve without prompting)
echoagent -p "Build the project" --allow "Bash"

# Combine: allow fetching docs sites, deny other URLs
echoagent --allow "WebFetch(domain:docs.rs)" --deny "WebFetch(*)"
```

`--allow` and `--deny` can be repeated to add multiple rules. Deny rules take precedence over allow rules. These flags work in both TUI and headless mode.

### Examples

```bash
# Simple question
echoagent -p "What does this project do?"

# Use a specific model
echoagent -p "Optimize this function" -m echo-agent-build

# Get JSON output for parsing
echoagent -p "List all TODO comments in the codebase" --output-format json

# Streaming JSON for real-time processing
echoagent -p "Explain the architecture" --output-format streaming-json

# Multi-turn conversation (session ID is returned in JSON output)
echoagent -p "Remember: the secret number is 42" --output-format json
echoagent -p "What's the secret number?" --resume <sessionId>

# Resume most recent session
echoagent -p "Continue where we left off" -c

# Run in a different directory
echoagent -p "Run the tests" --cwd ~/projects/other-app --always-approve
```

### Scripting with Named Sessions

For CI and automation, `-s/--session-id` lets you choose your own session ID:

```bash
# Start a session namespaced to a PR
echoagent -p "Review the changes in this PR" -s "critique-myrepo-pr-123"

# Continue in the same session
echoagent -p "Now check for security issues" -s "critique-myrepo-pr-123"
```

If the session exists it picks up where you left off; if not, a new one is created.
This differs from `--resume`, which errors when the session doesn't exist.

> **Note:** `-s/--session-id` is for headless mode (`-p/--single`) only.
> In the interactive TUI, use `/load` or `--resume`.

### Output Formats

**plain** (default) — Human-readable text:

```
Here's a summary of the codebase...
```

**json** — Single JSON object after completion:

```json
{
  "text": "Here's a summary of the codebase...",
  "stopReason": "EndTurn",
  "sessionId": "abc123",
  "requestId": "xyz789"
}
```

**streaming-json** — Newline-delimited JSON events:

```json
{"type":"text","data":"Here's"}
{"type":"text","data":" a summary"}
{"type":"thought","data":"Analyzing the directory structure..."}
{"type":"end","stopReason":"EndTurn","sessionId":"abc123","requestId":"xyz789"}
```

### Scripting Examples

```bash
# Pipe output to a file
echoagent -p "Generate a README" > README.md

# Parse JSON output with jq
echoagent -p "List files" --output-format json | jq -r '.text'

# CI/CD: automated code review
echoagent -p "Review changes for bugs and security issues." \
  --output-format json --always-approve | jq -r '.text' > review.md

# Pipeline: chain with other tools
git diff --staged | echoagent -p "Write a concise commit message for these changes"

# Batch: process multiple files
for file in src/*.js; do
  echoagent -p "Migrate $file from CommonJS to ES modules." --always-approve
done

# Pre-commit hook
echoagent -p "Review staged changes for obvious bugs. Reply OK if fine, or list issues." \
  --always-approve --output-format json | jq -r '.text' | grep -q "^OK" || exit 1
```

> **Note:** Headless mode starts a fresh session by default. Use `-s <id>` to maintain context across calls.

---

## Agent Mode

Run EchoAgent as an ACP (Agent Client Protocol) agent for integration with IDEs, editors, and custom tooling.

### stdio Transport

For direct integration with ACP clients:

```bash
echoagent agent stdio
```

Communication happens via JSON-RPC over stdin/stdout. This mode is used by:

- IDE extensions (Zed, Neovim, Emacs, etc.)
- Custom automation tools
- ACP client libraries

### Options

| Flag                  | Description                                                                         |
| --------------------- | ----------------------------------------------------------------------------------- |
| `-m, --model <MODEL>` | Override the default model ID (e.g., `echo-agent-build`)                           |
| `--always-approve`    | Start in always-approve mode (auto-approve all tool executions without confirmation) |
| `--reauth`            | Force re-authentication flow                                                        |

<details>
<summary><strong>Advanced: WebSocket Relay</strong></summary>

To expose the agent over the internet (instead of local network), run a WebSocket relay server and have the agent connect to it:

```bash
echoagent agent headless --echo-agent-ws-url wss://your-relay.example.com/ws
```

The agent connects OUT to your relay, and your web clients connect to the same relay. Useful for building web UIs where browsers can't spawn local processes.

</details>

---

## SSH Passthrough (`echoagent ssh`)

Use `echoagent ssh` instead of plain `ssh` when connecting to remote hosts in terminals that lack native support (e.g. Apple Terminal) for local OSC 52 clipboard interception.

```bash
# Basic usage (same args as ssh)
echoagent ssh user@host

# With SSH flags
echoagent ssh -t user@host
echoagent ssh -L 8080:localhost:8080 user@host

# With remote command
echoagent ssh user@host -- tmux attach
```

On macOS, if the terminal doesn't natively handle OSC 52, `echoagent ssh` runs SSH inside a local PTY that intercepts clipboard sequences and writes them to `pbcopy`. Both plain OSC 52 and tmux DCS passthrough are handled. Terminals with native OSC 52 (iTerm2, Ghostty, Kitty, WezTerm, Alacritty) get a plain `ssh` exec with no wrapper.

This runs entirely locally.

---

## Building with EchoAgent

EchoAgent can be used as an OpenAI-compatible chat completion backend. Choose between two integration modes:

| Mode         | Use Case                                                           |
| ------------ | ------------------------------------------------------------------ |
| **Headless** | Simple chat API, scripts, automation, OpenAI SDK drop-in           |
| **ACP SDK**  | IDE integrations, tool visibility, thought streams, permission UIs |

---

### Headless Mode (Simple Chat Completion)

Use headless mode for simple integrations. Spawns `echoagent -p` and parses JSON output.

#### Python - Headless

```python
import asyncio
import json
import os

class EchoAgentChat:
    """Simple OpenAI-compatible wrapper using headless mode."""

    def __init__(self, cwd="."):
        self.cwd = cwd
        self.env = {**os.environ}

    def _build_cmd(self, prompt, model, stream):
        return ["echoagent", "-p", prompt, "-m", model, "--cwd", self.cwd,
                "--output-format", "streaming-json" if stream else "json", "--always-approve"]

    async def create(self, messages, model="echo-agent-build", stream=False):
        prompt = messages[-1]["content"] if len(messages) == 1 else "\n".join(
            f"{m['role']}: {m['content']}" for m in messages
        )
        cmd = self._build_cmd(prompt, model, stream)

        if stream:
            return self._stream(cmd)

        proc = await asyncio.create_subprocess_exec(
            *cmd, env=self.env, stdout=asyncio.subprocess.PIPE
        )
        stdout, _ = await proc.communicate()
        data = json.loads(stdout.decode()) if stdout else {"text": ""}
        return {
            "choices": [{
                "message": {"role": "assistant", "content": data.get("text", "")},
                "finish_reason": "stop"
            }]
        }

    async def _stream(self, cmd):
        proc = await asyncio.create_subprocess_exec(
            *cmd, env=self.env, stdout=asyncio.subprocess.PIPE
        )
        async for line in proc.stdout:
            if not line.strip():
                continue
            event = json.loads(line)
            if event.get("type") == "text":
                yield {"choices": [{"delta": {"content": event["data"]}}]}
            elif event.get("type") == "end":
                yield {"choices": [{"delta": {}, "finish_reason": "stop"}]}


# Usage
async def main():
    client = EchoAgentChat(cwd=".")

    # Non-streaming
    response = await client.create([{"role": "user", "content": "What files are here?"}])
    print(response["choices"][0]["message"]["content"])

    # Streaming
    async for chunk in await client.create(
        [{"role": "user", "content": "List files"}], stream=True
    ):
        print(chunk["choices"][0]["delta"].get("content", ""), end="", flush=True)

asyncio.run(main())
```

#### TypeScript - Headless

```typescript
import { execa } from "execa";

class EchoAgentChat {
  constructor(private cwd = ".") {}

  private buildArgs(prompt: string, model: string, stream: boolean) {
    return [
      "-p",
      prompt,
      "-m",
      model,
      "--cwd",
      this.cwd,
      "--output-format",
      stream ? "streaming-json" : "json",
      "--always-approve",
    ];
  }

  async create(
    messages: { role: string; content: string }[],
    { model = "echo-agent-build", stream = false } = {},
  ) {
    const prompt =
      messages.length === 1
        ? messages[0].content
        : messages.map((m) => `${m.role}: ${m.content}`).join("\n");

    if (stream) return this.streamResponse(prompt, model);

    const { stdout } = await execa(
      "echoagent",
      this.buildArgs(prompt, model, false),
    );
    const data = JSON.parse(stdout || '{"text":""}');
    return {
      choices: [
        {
          message: { role: "assistant", content: data.text || "" },
          finish_reason: "stop",
        },
      ],
    };
  }

  async *streamResponse(prompt: string, model: string) {
    const proc = execa("echoagent", this.buildArgs(prompt, model, true));
    for await (const chunk of proc.stdout!) {
      for (const line of chunk.toString().split("\n").filter(Boolean)) {
        const event = JSON.parse(line);
        if (event.type === "text") {
          yield { choices: [{ delta: { content: event.data } }] };
        } else if (event.type === "end") {
          yield { choices: [{ delta: {}, finish_reason: "stop" }] };
        }
      }
    }
  }
}

// Usage
const client = new EchoAgentChat(".");

// Non-streaming
const response = await client.create([
  { role: "user", content: "What files are here?" },
]);
console.log(response.choices[0].message.content);

// Streaming
for await (const chunk of await client.create(
  [{ role: "user", content: "List files" }],
  { stream: true },
)) {
  process.stdout.write(chunk.choices[0].delta?.content || "");
}
```

---

### ACP SDK (Rich Agent Integration)

Use the Agent Client Protocol for full access to tool calls, thoughts, plans, and permissions.

#### Python - ACP SDK

```python
import asyncio
import json

class EchoAgentACPChat:
    """Rich OpenAI-compatible wrapper using ACP protocol."""

    def __init__(self, cwd="."):
        self.cwd = cwd
        self.proc = None
        self.session_id = None

    async def init(self):
        self.proc = await asyncio.create_subprocess_exec(
            "echoagent", "agent", "stdio",
            stdin=asyncio.subprocess.PIPE,
            stdout=asyncio.subprocess.PIPE
        )

        # Initialize
        await self._request("initialize", {
            "protocolVersion": "1",
            "clientCapabilities": {
                "fs": {"readTextFile": True, "writeTextFile": True},
                "terminal": True
            }
        })

        # Create session
        result = await self._request("session/new", {
            "cwd": self.cwd,
            "mcpServers": []
        })
        self.session_id = result["sessionId"]
        return self

    async def _request(self, method, params):
        msg = json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params})
        self.proc.stdin.write(f"{msg}\n".encode())
        await self.proc.stdin.drain()

        line = await self.proc.stdout.readline()
        return json.loads(line).get("result", {})

    async def create(self, messages, model="echo-agent-build", stream=False):
        prompt = [{"type": "text", "text": m["content"]} for m in messages]

        # For streaming, yield chunks as they arrive
        if stream:
            return self._stream(prompt)

        result = await self._request("session/prompt", {
            "sessionId": self.session_id,
            "prompt": prompt
        })
        return {
            "choices": [{
                "message": {"role": "assistant", "content": result.get("text", "")},
                "finish_reason": result.get("stopReason", "stop").lower()
            }]
        }

    async def _stream(self, prompt):
        # Send prompt request
        msg = json.dumps({
            "jsonrpc": "2.0", "id": 1,
            "method": "session/prompt",
            "params": {"sessionId": self.session_id, "prompt": prompt}
        })
        self.proc.stdin.write(f"{msg}\n".encode())
        await self.proc.stdin.drain()

        # Read streaming updates
        while True:
            line = await self.proc.stdout.readline()
            if not line:
                break

            data = json.loads(line)

            # Handle notifications
            if data.get("method") == "session/update":
                update = data["params"]["update"]
                session_update = update.get("sessionUpdate")

                if session_update == "agent_message_chunk":
                    yield {"choices": [{"delta": {"content": update["content"]["text"]}}]}
                elif session_update == "agent_thought_chunk":
                    yield {"choices": [{"delta": {"thought": update["content"]["text"]}}]}
                elif session_update == "tool_call":
                    yield {"choices": [{"delta": {"tool_call": {
                        "name": update["tool"],
                        "status": "pending"
                    }}}]}
                elif session_update == "plan":
                    yield {"choices": [{"delta": {"plan": update["entries"]}}]}

            # Handle final response
            elif "result" in data:
                yield {"choices": [{"delta": {}, "finish_reason": "stop"}]}
                break


# Usage
async def main():
    client = await EchoAgentACPChat(cwd=".").init()

    # Streaming with rich updates
    async for chunk in await client.create(
        [{"role": "user", "content": "Refactor the main function"}],
        stream=True
    ):
        delta = chunk["choices"][0]["delta"]
        if "content" in delta:
            print(delta["content"], end="", flush=True)
        if "thought" in delta:
            print(f"\n[Thinking: {delta['thought']}]", end="")
        if "tool_call" in delta:
            print(f"\n[Tool: {delta['tool_call']}]")
        if "plan" in delta:
            print(f"\n[Plan: {delta['plan']}]")

asyncio.run(main())
```

#### TypeScript - ACP SDK

```typescript
import { spawn, ChildProcess } from "child_process";
import * as readline from "readline";

class EchoAgentACPChat {
  private proc!: ChildProcess;
  private sessionId!: string;
  private rl!: readline.Interface;

  constructor(private cwd = ".") {}

  async init() {
    this.proc = spawn("echoagent", ["agent", "stdio"]);
    this.rl = readline.createInterface({ input: this.proc.stdout! });

    // Initialize
    await this.request("initialize", {
      protocolVersion: "1",
      clientCapabilities: {
        fs: { readTextFile: true, writeTextFile: true },
        terminal: true,
      },
    });

    // Create session
    const { sessionId } = await this.request("session/new", {
      cwd: this.cwd,
      mcpServers: [],
    });
    this.sessionId = sessionId;
    return this;
  }

  private async request(method: string, params: any): Promise<any> {
    return new Promise((resolve) => {
      const msg = JSON.stringify({ jsonrpc: "2.0", id: 1, method, params });
      this.proc.stdin!.write(msg + "\n");

      this.rl.once("line", (line) => {
        resolve(JSON.parse(line).result || {});
      });
    });
  }

  async create(
    messages: { role: string; content: string }[],
    { model = "echo-agent-build", stream = false } = {},
  ) {
    const prompt = messages.map((m) => ({ type: "text", text: m.content }));

    if (stream) return this.streamResponse(prompt);

    const result = await this.request("session/prompt", {
      sessionId: this.sessionId,
      prompt,
    });

    return {
      choices: [
        {
          message: { role: "assistant", content: result.text || "" },
          finish_reason: result.stopReason?.toLowerCase() || "stop",
        },
      ],
    };
  }

  async *streamResponse(prompt: { type: string; text: string }[]) {
    const msg = JSON.stringify({
      jsonrpc: "2.0",
      id: 1,
      method: "session/prompt",
      params: { sessionId: this.sessionId, prompt },
    });
    this.proc.stdin!.write(msg + "\n");

    for await (const line of this.rl) {
      const data = JSON.parse(line);

      if (data.method === "session/update") {
        const update = data.params.update;
        switch (update.sessionUpdate) {
          case "agent_message_chunk":
            yield { choices: [{ delta: { content: update.content?.text } }] };
            break;
          case "agent_thought_chunk":
            yield { choices: [{ delta: { thought: update.content?.text } }] };
            break;
          case "tool_call":
            yield {
              choices: [
                {
                  delta: {
                    tool_call: {
                      name: update.tool,
                      args: update.arguments,
                      status: "pending",
                    },
                  },
                },
              ],
            };
            break;
          case "plan":
            yield { choices: [{ delta: { plan: update.entries } }] };
            break;
        }
      } else if (data.result) {
        yield { choices: [{ delta: {}, finish_reason: "stop" }] };
        break;
      }
    }
  }
}

// Usage
const client = await new EchoAgentACPChat(".").init();

// Streaming with rich updates
for await (const chunk of await client.create(
  [{ role: "user", content: "Refactor main" }],
  { stream: true },
)) {
  const delta = chunk.choices[0].delta;
  if (delta.content) process.stdout.write(delta.content);
  if (delta.thought) console.log(`\n[Thinking: ${delta.thought}]`);
  if (delta.tool_call)
    console.log(`\n[Tool: ${JSON.stringify(delta.tool_call)}]`);
  if (delta.plan) console.log(`\n[Plan: ${JSON.stringify(delta.plan)}]`);
}
```

---

### ACP Protocol Reference

EchoAgent implements the [Agent Client Protocol (ACP)](https://agentclientprotocol.com), a standard for AI agent communication.

#### Architecture

```
┌─────────────────────────────────────────┐
│           ACP Client                    │
│  (IDE, Editor, Custom Application)      │
└──────────────────┬──────────────────────┘
                   │ JSON-RPC over stdio
┌──────────────────▼──────────────────────┐
│           echoagent agent stdio              │
│                                         │
│  ┌─────────┐  ┌─────────┐  ┌─────────┐  │
│  │ Session │  │  Tools  │  │   MCP   │  │
│  │ Manager │  │ Registry│  │ Servers │  │
│  └─────────┘  └─────────┘  └─────────┘  │
└─────────────────────────────────────────┘
```

#### SDKs

| Language   | Package                                                                                  |
| ---------- | ---------------------------------------------------------------------------------------- |
| TypeScript | [`@agentclientprotocol/sdk`](https://www.npmjs.com/package/@agentclientprotocol/sdk)     |
| Rust       | [`agent-client-protocol`](https://crates.io/crates/agent-client-protocol)                |
| Python     | [`agent-client-protocol-python`](https://github.com/PsiACE/agent-client-protocol-python) |
| Go         | [`acp-go-sdk`](https://github.com/coder/acp-go-sdk)                                      |
| Kotlin     | [`acp`](https://github.com/agentclientprotocol/kotlin-sdk)                               |

#### Resources

- [ACP Specification](https://agentclientprotocol.com/protocol/prompt-turn)
- [Protocol Introduction](https://agentclientprotocol.com/overview/introduction)

#### Compatible Clients

| Client                                                   | Status      |
| -------------------------------------------------------- | ----------- |
| [Zed](https://zed.dev/docs/ai/external-agents)           | ✓ Supported |
| [Neovim](https://neovim.io) (CodeCompanion, avante.nvim) | ✓ Supported |
| [Emacs](https://github.com/xenodium/agent-shell)         | ✓ Supported |
| [marimo notebook](https://github.com/marimo-team/marimo) | ✓ Supported |
| JetBrains                                                | Coming soon |

---

## Configuration

EchoAgent reads configuration from `~/.echo-agent/config.toml`. If the file doesn't exist, EchoAgent uses sensible defaults. You only need to specify values you want to override.

Each feature section below documents its own config. This section covers the general-purpose settings that don't have their own top-level section.

### General Settings

```toml
[cli]
auto_update = true                     # check for updates on launch

[models]
default = "echo-agent-4.6"                   # model used for new sessions
web_search = "echo-agent-4.6"                # model used by the web_search tool

[ui]
max_thoughts_width = 120               # max column width for reasoning display

[features]
support_permission = false             # prompt before tool execution
telemetry = false                      # anonymous usage telemetry (env: ECHO_AGENT_TELEMETRY_ENABLED)
feedback = false                       # feedback system (env: ECHO_AGENT_FEEDBACK_ENABLED)
lsp_tools = false                      # expose the lsp tool (see LSP Servers below)
codebase_indexing = true               # code graph indexing (true, false, or glob patterns)

[session]
auto_compact_threshold_percent = 85    # auto-compact at this % of context window
load_envrc = true                      # load .envrc environment variables into bash commands

[tools]
respect_gitignore = true               # filter gitignored files from tools (env: ECHO_AGENT_RESPECT_GITIGNORE)

[toolset.bash]
timeout_secs = 120.0                   # command timeout in seconds
output_byte_limit = 65536              # max output size (64KB)

[toolset.web_fetch]
proxy_endpoint = "https://proxy.example.com"   # egress proxy URL (all requests routed through it)
allowed_domains = ["docs.rs", "echo.agent"]           # override the built-in ~84-domain allowlist
```

### Telemetry

Configure telemetry destinations and credentials. Empty values disable the corresponding sink. Env vars take precedence over config values. Builds from the public source tree carry no telemetry defaults: `events_url`, `events_api_key`, and `mixpanel_token` are unset and `mixpanel_enabled` is `false`, so nothing is sent unless you supply values here or via env.

```toml
[telemetry]
events_url = "https://example.com/events"  # env: ECHO_AGENT_TELEMETRY_EVENTS_URL
events_api_key = "..."                      # env: ECHO_AGENT_TELEMETRY_EVENTS_API_KEY
mixpanel_token = "..."                      # env: ECHO_AGENT_TELEMETRY_MIXPANEL_TOKEN
mixpanel_enabled = true                     # env: ECHO_AGENT_TELEMETRY_MIXPANEL_ENABLED
trace_upload = true                         # env: ECHO_AGENT_TELEMETRY_TRACE_UPLOAD
```

When building from source, defaults can also be baked into the binary at compile time by setting `ECHO_AGENT_TELEMETRY_BUILD_EVENTS_URL`, `ECHO_AGENT_TELEMETRY_BUILD_EVENTS_API_KEY`, and `ECHO_AGENT_TELEMETRY_BUILD_MIXPANEL_TOKEN` in the build environment (providing a Mixpanel token this way also enables Mixpanel by default). Config-file and runtime env values override build-time defaults.

### LSP Servers

EchoAgent can connect to Language Server Protocol (LSP) servers configured in JSON files. LSP integration gives EchoAgent language-aware code intelligence while it works in your repository.

LSP support is used in two ways:

- **Passive diagnostics** — after edits, EchoAgent can surface language-server diagnostics such as errors and warnings.
- **The `lsp` tool** — EchoAgent can actively query the language server for `goToDefinition`, `findReferences`, `hover`, `goToImplementation`, `documentSymbol`, and `workspaceSymbol`.

Reference: [Language Server Protocol](https://microsoft.github.io/language-server-protocol/)

#### Config locations

EchoAgent looks for server definitions in:

- project config: `<repo>/.echo-agent/lsp.json`
- user config: `~/.echo-agent/lsp.json`

If the same server name appears in both places, the project config wins.

#### Tool enablement

Having an `lsp.json` file is enough for passive diagnostics. The model-visible `lsp` tool is exposed when both of these are true:

- LSP tools are enabled (`ECHO_AGENT_LSP_TOOLS=1` or `[features] lsp_tools = true`)
- the merged LSP configuration is non-empty

Enable the tool for one run:

```bash
ECHO_AGENT_LSP_TOOLS=1 echoagent
```

Or enable it in config:

```toml
[features]
lsp_tools = true
```

If LSP tools are enabled but no usable server config is found, EchoAgent emits a non-fatal warning in logs and continues without the `lsp` tool. If config exists but every server fails to start, the tool may still be present and will fail on first use with a startup error.

#### Example `lsp.json`

```json
{
  "typescript": {
    "command": "typescript-language-server",
    "args": ["--stdio"],
    "extensionToLanguage": {
      ".ts": "typescript",
      ".tsx": "typescriptreact"
    },
    "startupTimeout": 30000
  }
}
```

#### Required fields

| Field | Description |
|-------|-------------|
| `command` | Server binary to execute. For `stdio`, this must be available in `PATH` or be an absolute path. |
| `extensionToLanguage` | Maps file extensions to LSP language IDs. |

#### Optional fields

| Field | Description |
|-------|-------------|
| `args` | Command-line arguments for the server process. |
| `transport` | `stdio` (default) or `socket`. |
| `env` | Extra environment variables for the server process. |
| `initializationOptions` | JSON passed during LSP initialize. |
| `settings` | Configuration sent via workspace settings updates. |
| `workspaceFolder` | Override workspace folder path sent to the server. |
| `workspaceOpen` | Solution or projects to load, for servers that need to be told explicitly (see below). |
| `startupTimeout` | Max startup wait in milliseconds before startup is considered failed. |
| `shutdownTimeout` | Max graceful shutdown wait in milliseconds. |
| `restartOnCrash` | Whether to restart the server after a crash. |
| `maxRestarts` | Maximum restart attempts before giving up. |

#### Telling a server which solution to load (`workspaceOpen`)

Most servers work out what to analyze from the workspace folder. A few do not,
and instead load their workspace through a protocol extension. The C# server
(`Microsoft.CodeAnalysis.LanguageServer`, "Roslyn") is the notable one: on its
own it treats every file as a loose "miscellaneous file" and reports no
project-level diagnostics at all, until it is told to open a solution or a set
of projects.

```json
{
  "csharp": {
    "command": "dotnet",
    "args": [
      "/path/to/Microsoft.CodeAnalysis.LanguageServer.dll",
      "--stdio",
      "--logLevel", "Warning",
      "--extensionLogDirectory", "/tmp/roslyn-logs"
    ],
    "extensionToLanguage": { ".cs": "csharp" },
    "workspaceOpen": { "solution": "MyApp.sln" },
    "startupTimeout": 60000
  }
}
```

Use `"projects": ["src/App/App.csproj", "src/Lib/Lib.csproj"]` instead of
`"solution"` when there is no solution file. Paths may be absolute or relative
to the workspace root. Wrappers such as `roslyn-language-server` already send
these notifications themselves, in which case `workspaceOpen` can be omitted.

Note that `--logLevel` is required by that server, and that anything more
verbose than `Warning` makes it stream every internal log line to the client.

#### Installing language servers

EchoAgent does not bundle language server binaries. You must install the server yourself and make sure the configured `command` is runnable on your machine.

Examples:

| Language | Server | Install example |
|----------|--------|-----------------|
| TypeScript | `typescript-language-server` | `npm install -g typescript-language-server typescript` |
| Python | `pyright` | `npm install -g pyright` or `pip install pyright` |
| Rust | `rust-analyzer` | Install `rust-analyzer` using your platform's recommended method |

#### Notes

- Passive diagnostics do **not** require `ECHO_AGENT_LSP_TOOLS=1`; they run whenever an applicable server is configured and starts successfully.
- Passive diagnostics are currently driven by `search_replace` edits; they are not a general watcher for arbitrary shell or git mutations in the workspace.
- The `lsp` tool is intentionally hidden when disabled or unconfigured so the model does not plan around unavailable capabilities.
- Same-workspace subagents reuse the parent session's live LSP runtime instead of starting a duplicate server pool.
- That reuse means the child inherits the parent's LSP server set for the shared workspace; child-local LSP config differences are not loaded in the reused-runtime path.

### Enterprise Deployment

A complete `config.toml` for an enterprise deployment with external auth, corporate proxy, and telemetry disabled:

```toml
[cli]
auto_update = false

[auth]
auth_provider_command = "/usr/local/bin/my-company-auth-provider"
auth_provider_label = "Acme Corp"
auth_token_ttl = 3600               # if your provider outputs bare tokens

[models]
default = "company-echo-agent"

[model.company-echo-agent]
model = "echo-agent-build"
base_url = "https://echo-agent-proxy.acme.com/"
name = "EchoAgent Build Latest (Proxy)"
context_window = 256000

[features]
support_permission = false
telemetry = false

[toolset.bash]
timeout_secs = 120.0
```

With this config, `echoagent` runs your auth binary, stores the token, and routes inference through your corporate proxy. See [Authentication](#authentication) for full auth setup details.

---

## AGENTS.md

Add project-specific instructions by creating an agent rules file (e.g., `AGENTS.md`). EchoAgent reads these files and appends their contents to the system prompt.

EchoAgent scans for agent rules in this order:

1. `~/.echo-agent/` (global rules)
2. If inside a git repo: every directory from the repo root → current working directory (inclusive)
3. If **not** inside a git repo: only the current working directory

Within each directory, EchoAgent checks for these filenames:

- `Agents.md`, `Claude.md`, `AGENT.md`, `AGENTS.md`

Ordering matters: files found later (deeper directories) come last, so they effectively take precedence if instructions conflict. Files ignored by gitignore are skipped. Each file is capped at 10,000 characters (truncated with a warning if exceeded).

> **Note:** The `--rules` flag appends _additional_ rules on top of any discovered agent files, so you can combine both for session-specific customization.

---

## Skills

Skills are reusable prompt packages that extend EchoAgent with specialized workflows, domain knowledge, and tool integrations. Use them to encode repeatable procedures that would otherwise require re-explaining each session.

### Skill Locations

EchoAgent discovers skills from these directories (in priority order):

| Location                    | Scope | Priority |
| --------------------------- | ----- | -------- |
| `./.echo-agent/skills/`           | Local | Highest  |
| `<repo_root>/.echo-agent/skills/` | Repo  | Medium   |
| `~/.echo-agent/skills/`           | User  | Lowest   |
| `~/.claude/skills/`         | User  | Lowest   |

Skills with the same name are deduplicated — higher priority locations override lower ones.

Repo-scoped skills (Local and Repo) respect `.gitignore` and are filtered out if ignored. User-scoped skills (`~/.echo-agent/skills/`) are outside the repo and never filtered.

### Configuration

Add extra skill directories or exclude paths via `[skills]` in config.toml:

```toml
[skills]
paths = ["~/my-team-skills"]          # additional directories to scan
ignore = ["~/my-team-skills/wip"]     # paths to exclude
```

### Creating a Skill

Each skill lives in its own directory with a `SKILL.md` file:

```
~/.echo-agent/skills/
└── commit/
    └── SKILL.md
```

**SKILL.md format:**

```markdown
---
name: commit
description: Create well-formatted git commits following conventional commit standards. Use when the user wants to commit changes or asks for /commit.
---

# Git Commit Skill

Review staged changes and create a commit with a clear, conventional message.

## Steps

1. Run `git diff --staged` to see changes
2. Summarize what changed and why
3. Create commit message following conventional commits format
4. Run `git commit -m "..."` with the message
```

**Required frontmatter fields:**

| Field         | Description                                                                  |
| ------------- | ---------------------------------------------------------------------------- |
| `name`        | Skill identifier (lowercase, hyphens, max 64 chars)                          |
| `description` | What the skill does and when to use it—this is how EchoAgent decides to invoke it |

### Using Skills

**In the TUI:**

```bash
/skills              # List available skills
/skills commit       # Inject the "commit" skill into context
```

**The model can also invoke skills automatically** when it recognizes a relevant task. The skill's `description` field determines when this happens.

**Slash command shorthand:**

Users can reference skills as `/skill-name` (e.g., `/commit`). When you see this pattern, EchoAgent invokes the corresponding skill.

> **Tip:** The `description` field is critical — it determines when EchoAgent automatically invokes the skill. Be specific about trigger phrases and use cases.

---

## Agent Profiles

Agent profiles control the system prompt, toolset, and behavior of a session. A profile is a `.md` file with YAML frontmatter, or a named agent discovered from disk.

EchoAgent discovers agent definitions from `.echo-agent/agents/` (project), `~/.echo-agent/agents/` (user), and built-in agents. Priority (highest wins):

1. `--agent-profile <PATH>` CLI flag
2. `[agent]` section in `config.toml`
3. `ECHO_AGENT_AGENT` env var
4. Default `echo-agent-build` agent

```toml
# ~/.echo-agent/config.toml
[agent]
name = "my-custom-agent"             # Discovered by name
# definition = "/path/to/agent.md"   # OR: explicit path
```

```bash
echoagent --agent-profile ./my-agent.md
# or
export ECHO_AGENT_AGENT="my-custom-agent"
```

---

## Subagents

Subagents spawn independent child sessions that handle tasks in parallel. Each child has its own context window and can optionally inherit the parent's conversation history. Enabled by default.

### Disabling

```bash
export ECHO_AGENT_SUBAGENTS=0              # Environment variable
```

```toml
# ~/.echo-agent/config.toml
[subagents]
enabled = false
```

### Toggles and Model Overrides

Disable specific subagent types while keeping the system enabled, or route them to different models:

```toml
[subagents.toggle]
explore = true                       # default — omitted agents are enabled
plan = false                         # disable plan subagent

[subagents.models]
explore = "echo-agent-build"              # route explore to a lighter model
```

By default a subagent inherits the parent session's model. Only an explicit
per-agent pin overrides that: `[subagents.models].<agent>` (highest priority),
then the agent definition's `model`. Both pins apply unconditionally,
regardless of which model the parent is on.

### Roles and Personas

Roles define reusable capability/model defaults. Personas layer tone and behavior instructions onto the child prompt.

```toml
[subagents.roles.researcher]
description = "Deep research agent"
default_capability_mode = "read-only"
model = "echo-agent-build"
prompt_file = ".echo-agent/prompts/researcher.md"

[subagents.personas.concise]
instructions = "Be extremely concise. No filler words."
# instructions_file = ".echo-agent/personas/concise.md"  # or load from file
```

Both are also discovered from `.echo-agent/roles/*.toml` and `.echo-agent/personas/*.toml` files respectively. If a requested persona is not found, the spawn fails (fail-closed).

---

## Plugins

Plugins extend EchoAgent with additional tools, skills, and MCP servers from external packages.

### Plugin Locations

| Location                    | Scope   |
| --------------------------- | ------- |
| `.echo-agent/plugins/`            | Project |
| `~/.echo-agent/plugins/`          | User    |
| `--plugin-dir <PATH>` (CLI) | Session |

### Configuration

```toml
# ~/.echo-agent/config.toml
[plugins]
paths = ["~/my-plugins/custom-tools"]       # additional plugin directories
disabled = ["user/a1b2c3d4/noisy-plugin"]   # plugin IDs to skip
```

Manage plugins at runtime with `/plugins list`, `/plugins reload`, or `/plugins trust <path>`.

---

## Hooks

Hooks run project scripts on tool and session lifecycle events (pre/post-tool-use, session start/end). Projects must be explicitly trusted before their hooks execute.

EchoAgent discovers hooks from `.echo-agent/hooks/` in the project directory. Manage them with:

```
/hooks-list              # show hooks loaded in this session
/hooks-trust             # trust this project for hook execution
/hooks-add <path>        # add a custom hook file or directory
```

### Hooks in config files

Hooks can also be defined directly in the config layers, so they can be
distributed with your other configuration instead of as separate JSON files. Add
a `[[hooks.<Event>]]` table to `config.toml` (your own), `managed_config.toml`, or
`requirements.toml`:

```toml
[[hooks.PreToolUse]]
matcher = "Bash|Write|Edit"
  [[hooks.PreToolUse.hooks]]
  type = "command"
  command = "/opt/guard/pretooluse.sh"   # use an absolute path
  timeout = 10
```

The schema matches the JSON `hooks` object used in hook files. Hooks are read from
every layer and combined additively: a lower-priority layer can add hooks but
never removes or replaces another layer's block. Each hook's `/hooks-list` name is
prefixed with the layer it came from (for example `managed:` or
`requirements/user:`).

Config-layer hooks are convenience distribution, not an enforcement boundary: on
an unmanaged device a user can still edit these files. Tamper-resistant,
admin-enforced hooks are tracked separately.

---

## Custom Models

Add custom model endpoints to use alternative providers or self-hosted models. You can also override built-in models with custom settings.

### Model Configuration

The name in the TOML header (`my-model` in `[model.my-model]`) is what appears in the model picker. The `model` field is the identifier sent to the API. If `model` is omitted, the header name is sent to the API directly.

```toml
[model.my-model]
model = "model-id"                    # Model identifier sent to API
base_url = "https://api.example.com/v1"  # OpenAI-compatible endpoint
name = "Display Name"                 # Shown in model picker
description = "Model description"     # Optional description
api_key = "sk-..."                    # API key for this provider (optional)
env_key = "OPENAI_API_KEY"            # Env var(s) holding the API key (string or array; first set wins)
auth_provider = "corp-gateway"        # Named credential helper for rotating tokens (optional)
temperature = 0.7                     # Sampling temperature (0.0-2.0)
top_p = 0.95                          # Nucleus sampling parameter
max_completion_tokens = 8192          # Max tokens per response
context_window = 256000               # Total context window in tokens (for auto-compact)
```

**Credential resolution order:** `api_key` → `env_key` → cached `auth_provider` token (terminal: a cache miss resolves to no credential, never the session token) → session token → `ECHO_AGENT_API_KEY`. See [Per-Model Auth Providers](#per-model-auth-providers).

The `context_window` parameter is used to calculate when auto-compact should trigger. If not specified, EchoAgent falls back to built-in defaults for known models.

### Overriding Built-in Models

You can override specific fields of built-in models without redefining everything. Only specify the fields you want to change:

```toml
# Override just the API key for a default model
[model.echo-agent-build]
api_key = "my-api-key"

# Override temperature and add a custom API key
[model.echo-agent-4.20-0309-reasoning]
temperature = 0.5
api_key = "sk-custom"
```

**How it works:** When you override a built-in model, EchoAgent starts with the default configuration (including the correct `base_url` from your `[endpoints]` setting), then applies only the fields you specify. Unspecified fields inherit from the default.

**Priority order:**
1. Your config (`[model.*]`) — highest priority
2. Prefetched models from remote `/v1/models`
3. Hardcoded defaults — lowest priority

**Web search model:** Set `[models] web_search`, `ECHO_AGENT_WEB_SEARCH_MODEL`, or `--web-search-model` to point the `web_search` tool at a different model. The target endpoint must support the Responses API and web search.

> **Overriding with a custom model:** Setting `[models] web_search` alone is not
> enough if the model isn't already in the catalog (built-in defaults or
> `echoagent models` output). You also need a `[model.*]` entry so EchoAgent knows
> how to reach it. Without both, web search is silently disabled.
>
> ```toml
> [models]
> web_search = "my-custom-model"       # 1. tell web search which model to use
>
> [model.my-custom-model]              # 2. tell EchoAgent how to reach it
> model = "my-custom-model"
> api_backend = "responses"            # required — web search uses the Responses API
> # base_url, api_key, env_key optional — defaults to cli-chat-proxy
> ```

### Examples

**OpenAI-compatible endpoint:**

```toml
[model.local-llama]
model = "llama-3.1-70b"
base_url = "http://localhost:8080/v1"
name = "Local Llama"
temperature = 0.8
```

**Ollama:**

```toml
[model.ollama-codellama]
model = "codellama"
base_url = "http://localhost:11434/v1"
name = "CodeLlama (Ollama)"
```

**Together AI:**

```toml
[model.together-mixtral]
model = "mistralai/Mixtral-8x7B-Instruct-v0.1"
base_url = "https://api.together.xyz/v1"
name = "Mixtral 8x7B"
env_key = "TOGETHER_API_KEY"
```

**OpenAI:**

```toml
[model.gpt-4o]
model = "gpt-4o"
base_url = "https://api.openai.com/v1"
name = "GPT-4o"
env_key = "OPENAI_API_KEY"
```

### Using Custom Models

```bash
# List available models (including custom)
echoagent models

# Use in TUI via slash command
/model my-model

# Use in headless mode
echoagent -p "Hello" -m my-model

# Set as default
# In config.toml:
[models]
default = "my-model"
```

### Custom Models Endpoint

Point EchoAgent at a custom OpenAI-compatible `/v1/models` endpoint instead of the default cli-chat-proxy. Useful when models are served behind a corporate gateway or self-hosted inference stack.

**Environment variables:**

| Variable | Required | Description |
|----------|----------|-------------|
| `ECHO_AGENT_MODELS_BASE_URL` | Yes | Base URL for inference / chat completions (e.g. `https://api.acme.com/v1`). The model list is fetched from `{base_url}/models` automatically |
| `ECHO_AGENT_API_KEY` | Yes | API key sent as `Authorization: Bearer` to the custom endpoint |
| `ECHO_AGENT_MODELS_LIST_URL` | No | Override the model list URL if it differs from `{base_url}/models` |

**Setup:**

```bash
export ECHO_AGENT_MODELS_BASE_URL="https://api.acme.com/v1"
export ECHO_AGENT_API_KEY="echo-agent-..."
echoagent
```

EchoAgent fetches the model list from `{ECHO_AGENT_MODELS_BASE_URL}/models` on startup and sends inference requests to `ECHO_AGENT_MODELS_BASE_URL`. This follows the standard OpenAI-compatible convention used by OpenAI, Anthropic, OpenRouter, Groq, Together.ai, and others.

If your model list endpoint differs from `{base_url}/models`, set `ECHO_AGENT_MODELS_LIST_URL` explicitly.

**Combining with `[endpoints]` config:** You can also set endpoints in `~/.echo-agent/config.toml`:

```toml
[endpoints]
models_base_url = "https://api.acme.com/v1"

# Override just the API key for a specific model
[model.echo-agent-build]
api_key = "my-api-key"
```

When using `[endpoints]` with partial model overrides, the `base_url` is inherited from the endpoints config — you don't need to specify it in each `[model.*]` section.

**Auth behavior:** When `models_base_url` is set, EchoAgent uses API key auth (`Authorization: Bearer`) instead of session auth. `echoagent login` is not required — only the API key.

---

## MCP Servers

Extend EchoAgent's capabilities with [Model Context Protocol](https://modelcontextprotocol.io) servers.

### Configuration

MCP servers are configured in `~/.echo-agent/config.toml`:

```toml
[mcp_servers.<name>]
command = "/path/to/server"           # Server executable
args = ["--flag", "value"]            # Command arguments
env = { VAR = "value" }               # Environment variables
headers = { "X-Header" = "value" }    # Optional HTTP headers (Streamable HTTP)
enabled = true                        # Enable/disable (default: true)
startup_timeout_sec = 30              # Init timeout (default: 30)
tool_timeout_sec = 60                 # Tool call timeout (default: 60)
tool_timeouts = { create_issue = 120, search = 30 }  # Per-tool timeout overrides (seconds)
```

### Project-Scoped MCP Servers

MCP servers can also be configured per-project in `.echo-agent/config.toml`. EchoAgent walks from the current directory up to the git repo root, loading `.echo-agent/config.toml` at each level:

| Location                        | Scope             | Priority |
| ------------------------------- | ----------------- | -------- |
| `~/.echo-agent/config.toml`           | All projects      | Lowest   |
| `<repo-root>/.echo-agent/config.toml` | This repository   | ↑        |
| `<cwd>/.echo-agent/config.toml`       | Current directory | Highest  |

If a project defines a server with the same name as a global one, the project version **replaces** it entirely (fields are not merged — omitted fields get defaults, not the global values). Servers defined only in the global config are unaffected.

**Example:** commit a `.echo-agent/config.toml` in your repo to share MCP servers across the team:

```
my-project/
├── .echo-agent/
│   └── config.toml
├── src/
└── ...
```

```toml
# .echo-agent/config.toml
[mcp_servers.linear]
command = "npx"
args = ["-y", "mcp-remote", "https://mcp.linear.app/mcp"]
```

If you also have a `linear` server in `~/.echo-agent/config.toml`, the project version replaces it entirely.

> **Note:** Only `[mcp_servers]` is supported in project-scoped `.echo-agent/config.toml`. Other config sections (models, etc.) are only read from `~/.echo-agent/config.toml`.

### Tool Naming

MCP tools are namespaced with the server name:

- Server `filesystem` with tool `read_file` → `filesystem__read_file`
- Server `github` with tool `create_issue` → `github__create_issue`

### Example Servers

**Filesystem access:**

```toml
[mcp_servers.filesystem]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/path/to/allowed/directory"]
```

**GitHub integration:**

```toml
[mcp_servers.github]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]
env = { GITHUB_PERSONAL_ACCESS_TOKEN = "ghp_xxxxxxxxxxxx" }
```

**Postgres database:**

```toml
[mcp_servers.postgres]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-postgres", "postgresql://user:pass@localhost/db"]
```

**Custom server:**

```toml
[mcp_servers.my-tools]
command = "/usr/local/bin/my-mcp-server"
args = ["--config", "/etc/my-mcp.json"]
startup_timeout_sec = 30
tool_timeout_sec = 120
```

**Streamable HTTP with session id header:**

```toml
[mcp_servers.my-http-mcp]
url = "http://localhost:5000/api/mcp"
headers = { "x-session-id" = "{{session_id}}" }
```

### Available MCP Servers

See the [MCP Server Registry](https://github.com/modelcontextprotocol/servers) for community servers:

- Filesystem, Git, GitHub, GitLab
- PostgreSQL, SQLite, Redis
- Slack, Discord, Linear
- Puppeteer, Playwright
- And many more

---

## Memory

> **Experimental:** enable with `ECHO_AGENT_MEMORY=1`, `[memory] enabled = true`, or managed remote settings.

Cross-session memory lets EchoAgent remember facts, decisions, code patterns, and debugging workflows across separate sessions in the same project.

### How it works

Memory is stored as Markdown files under `~/.echo-agent/memory/`:
- **Global** (`~/.echo-agent/memory/MEMORY.md`) — facts that apply across all your projects
- **Workspace** (`~/.echo-agent/memory/<project-slug>-<hash8>/MEMORY.md`) — project-specific conventions and context
- **Session logs** (`~/.echo-agent/memory/<project-slug>-<hash8>/sessions/`) — per-session summaries

Workspace directories are suffixed with a short hash for uniqueness (e.g. `echo-agent-a3f7b2c9/`). The hash is derived from the git remote URL so all clones and worktrees of the same repository share the same memory directory.

An SQLite index enables fast hybrid search (FTS5 keyword + optional vector KNN) across all memory files.

### Enabling memory

```bash
# Environment variable (persists for the shell session)
export ECHO_AGENT_MEMORY=1
echoagent

# Config file (persists permanently)
# ~/.echo-agent/config.toml
[memory]
enabled = true
```

### What gets saved automatically

At the end of each session, EchoAgent saves a **structured metadata summary** to the daily session log:
- Message counts (user / assistant / tool)
- Topics — the first few real user prompts from the session
- Tool-usage breakdown (e.g., `read_file: 4, search_replace: 3`)
- File paths that were read or edited
- Date and session ID

Shell commands are intentionally **not** recorded in automatic saves — command
strings often embed secrets (tokens, API keys, DSNs) and auto-save runs silently.
For command history, use `/flush`, which is user-initiated and produces an
LLM-generated summary rather than raw verbatim output.

This summary is searchable in future sessions but does **not** capture full content or reasoning.

### Capturing rich knowledge with `/flush`

For richer capture — decisions, patterns, debugging workflows, API discoveries — use `/flush` in the TUI. This triggers an LLM-generated summary of the current session's most important content and writes it to a dated session log under `~/.echo-agent/memory/<project-slug>-<hash8>/sessions/`, where it is indexed and searchable in future sessions.

Use `/flush` when you want to preserve important context before compaction or at any point during a productive session.

```
/flush
```

### Appending to memory manually

You can append facts directly from the TUI without leaving the session:

```
/memory workspace Use Rust for all backend services.
/memory global Prefer 2-space indentation in TypeScript.
/memory global Preferred editor: VS Code with Vim keybindings.
```

Omit `workspace` or `global` and it defaults to workspace scope.

### Searching memory

EchoAgent searches memory automatically on the first turn of each session and after compaction. The first-turn injection can be disabled or given its own score threshold under `[memory.initial_injection]`. You can also invoke `memory_search` and `memory_get` directly via the model prompt:

```
Search memory for "auth middleware patterns"
Read my workspace MEMORY.md
```

### CLI commands

```bash
# Open workspace MEMORY.md in $EDITOR / $VISUAL
echoagent memory edit

# Open global MEMORY.md
echoagent memory edit --global

# Show memory statistics: file count, chunk count, and index size
echoagent memory stats
```

### Configuration reference

Key options under `[memory]` in `~/.echo-agent/config.toml`:

| Key | Default | Description |
|-----|---------|-------------|
| `enabled` | `false` | Enable memory (can also be set via CLI flag or env var) |
| `session.save_on_end` | `true` | Write the lightweight metadata summary on session end |
| `watcher.enabled` | `true` | Watch `~/.echo-agent/memory/` for external edits and reindex on search |
| `search.max_results` | `6` | Default number of memory results to return |
| `search.min_score` | `0.35` | Minimum relevance score threshold for explicit memory search and recovery paths |
| `initial_injection.enabled` | `true` | Enable automatic first-turn memory injection |
| `initial_injection.min_score` | `0.0` | Override score threshold for first-turn injection (`0.0` preserves historical no-filter behavior) |
| `embedding.model` | *(unset)* | Embedding model for vector search; unset disables embeddings |
| `embedding.dimensions` | `1024` | Embedding vector dimensions |

### Observability

When first-turn memory injection runs, EchoAgent emits the `echo-agent-shell-memory_injection`
telemetry event. It includes:
- whether the greeting fallback query path was used
- result counts and top score
- the configured first-turn threshold via `configured_min_score`

---

## Sandbox

EchoAgent can restrict what the agent process and its spawned commands can access on
your filesystem and network using OS-level kernel primitives (Landlock on Linux,
Seatbelt on macOS). This is off by default.

### Quick Start

```bash
# Run with workspace sandbox (read everywhere, write only to CWD + /tmp)
echoagent --sandbox workspace

# Read-only mode (agent can read but not write anything)
echoagent --sandbox read-only

# Maximum isolation (read/write CWD only, no child network)
echoagent --sandbox strict
```

### Built-in Profiles

| Profile         | FS Read            | FS Write                  | Child Network | Use Case                 |
| --------------- | ------------------ | ------------------------- | ------------- | ------------------------ |
| `off` (default) | Unrestricted       | Unrestricted              | Unrestricted  | No sandbox               |
| `workspace`     | Everywhere         | CWD + `/tmp` + `~/.echo-agent/` | Allowed       | Normal development       |
| `read-only`     | Everywhere         | `~/.echo-agent/` only           | Blocked       | Exploration, code review |
| `strict`        | CWD + system paths | CWD + `/tmp` + `~/.echo-agent/` | Blocked       | Untrusted code           |

Sensitive paths (`~/.ssh/`, `~/.aws/`, `~/.gnupg/`, `~/.echo-agent/auth/`) are always
write-protected regardless of profile.

### Custom Profiles

Create `~/.echo-agent/sandbox.toml` (global) or `.echo-agent/sandbox.toml` (per-project):

```toml
[profiles.devbox]
# Start from a built-in profile, then add overrides
extends = "workspace"
restrict_network = true

# Paths the agent can read but NOT write/delete
read_only = ["/data"]

# Additional writable paths (literal directory grants — no globs;
# trailing /** is treated as the parent directory)
read_write = ["/tmp/scratch"]

# Paths denied entirely
deny = ["/data/shared-secrets"]
```

Use it:

```bash
echoagent --sandbox devbox
```

### How It Works

The sandbox is applied to the **entire echoagent process** at startup using kernel
primitives — not per-command wrapping. This means all tool operations are
covered:

- `read_file`, `search_replace`, `list_dir` — restricted by Landlock/Seatbelt in-process
- `bash` commands, `grep` (rg) — child processes inherit FS restrictions automatically
- Network — child processes can be blocked via seccomp (Linux)

The sandbox is **irreversible** once applied. This is a security feature — the
model cannot convince the agent to relax restrictions at runtime.

### Current Limitations

- **Platform support**: Sandbox enforcement uses Landlock on Linux (kernel ≥ 5.13)
  and Seatbelt on macOS. If the sandbox cannot be applied (e.g., unsupported
  kernel, missing entitlements), EchoAgent logs a warning and continues without
  enforcement.

- **Network restrictions are partial**: Profiles with `restrict_network` block
  network in **child processes** (bash commands, scripts) via seccomp, but
  built-in tools that make HTTP requests in-process (web search, LLM API) are
  not affected. The agent needs network access to function, so process-level
  network cannot be blocked.

### Event Logging

Sandbox events (profile applied, violations) are logged to `~/.echo-agent/sandbox-events.jsonl`
for telemetry and debugging.

---

## Introspection

Use `echoagent inspect` to see everything EchoAgent discovers in the current directory:

```bash
echoagent inspect          # human-readable output
echoagent inspect --json   # machine-readable JSON
```

The output shows all loaded configuration organized by type:

- **Project Instructions** — AGENTS.md / CLAUDE.md files with token counts
- **Skills** — from `.echo-agent/skills/`, `~/.echo-agent/skills/`, plugins, and config paths
- **Agents** — built-in, user-defined, and plugin-provided subagents
- **Plugins** — discovered plugins with what each provides (skills, agents, hooks, MCPs)
- **MCP Servers** — from `config.toml`, plugins, `~/.claude.json`, and `.mcp.json`
- **LSP Servers** — language servers from `lsp.json` and plugins
- **Hooks** — project and plugin hooks
- **Permissions, Config Sources** — which config files are active

Plugin-provided components appear in their respective sections with a `[plugin: name]` tag, so you can see at a glance where each skill, MCP server, or agent originates.

---

## Claude Code Compatibility

EchoAgent automatically discovers configuration from Claude Code directories alongside native `.echo-agent/` paths. No extra setup is needed.

### What is picked up

| Component         | Claude Code location                                 | How EchoAgent uses it                 |
| ----------------- | ---------------------------------------------------- | -------------------------------- |
| **Skills**        | `.claude/skills/`, `~/.claude/skills/`               | Loaded as skills (same as `.echo-agent/skills/`) |
| **Agents**        | `.claude/agents/`, `~/.claude/agents/`               | Loaded as subagents              |
| **Plugins**       | `.claude/plugins/`, `~/.claude/plugins/`             | Discovered with all components   |
| **Installed plugins** | `~/.claude/plugins/installed_plugins.json`        | Each `installPath` is loaded     |
| **Marketplaces**  | `~/.claude/plugins/known_marketplaces.json`          | Plugin dirs from `installLocation` |
| **MCP servers**   | `~/.claude.json`, `.mcp.json`                        | Loaded alongside `config.toml`   |
| **Project rules** | `CLAUDE.md`, `.claude/CLAUDE.md`                     | Loaded as project instructions   |
| **Permissions**   | `.claude/settings.json`, `.claude/settings.local.json` | Fallback when no TOML config   |

### Plugin components

Claude Code plugins can provide skills (`skills/`), commands (`commands/`), agents (`agents/`), hooks (`hooks/hooks.json`), MCP servers (`.mcp.json`), and LSP servers (`.lsp.json`). All component types are discovered and used by EchoAgent at runtime.

---

## Built-in Tools

EchoAgent includes these tools by default:

| Tool             | Description                                                    |
| ---------------- | -------------------------------------------------------------- |
| `read_file`      | Read file contents with line numbers                           |
| `search_replace` | Make precise edits to files                                    |
| `grep_search`    | Search with regex patterns (ripgrep)                           |
| `list_dir`       | List directory contents                                        |
| `bash`           | Execute shell commands                                         |
| `web_search`     | Search the web for up-to-date information                      |
| `web_fetch`      | Fetch a specific URL and return its content as markdown        |
| `todo_write`     | Create and manage task lists                                   |
| `task`           | Launch subagent sessions (requires `--subagents`)              |
| `kill_task`      | Terminate a running background task or subagent                |
| `get_task_output` | Get output and status from a background task or subagent      |
| `memory_search`  | Search cross-session memory (requires memory enabled) |
| `memory_get`     | Read a memory file by path                                     |
| `search_tool`    | Discover available integration tools (MCP)                     |
| `use_tool`       | Call an integration tool discovered via `search_tool`           |
| `lsp`            | Code intelligence via language servers (requires `lsp_tools`)  |

### Controlling Available Tools

In headless mode, you can restrict or remove tools with the `--tools` (allowlist) and `--disallowed-tools` (denylist) flags. See [Headless Mode](#headless-mode) for details and examples.

In agent profiles, use the `tools` and `disallowedTools` frontmatter fields:

```yaml
---
tools:
  - read_file
  - grep_search
  - list_dir
disallowedTools:
  - web_search
  - Agent(explore)
---
```

### `web_fetch`

Fetch a specific URL and return its content as markdown. **Disabled by default** — enable with `ECHO_AGENT_WEB_FETCH=1`. 

When no custom `allowed_domains` is set, the tool permits a default allowlist of useful documentation sites (EchoAgent, language docs, frameworks, cloud providers, databases, etc.). Domains not on the allowlist prompt the user for approval; `--always-approve` auto-approves all. Domain matching is case-insensitive, strips `www.` prefixes, and supports path-scoped entries (e.g. `echo.agent/company`).

---

## Session Persistence

EchoAgent automatically persists conversations to disk. This works across all modes: TUI, headless, and agent stdio.

### Storage Layout

Sessions are stored under `~/.echo-agent/sessions/`, organized by URL-encoded working directory:

```
~/.echo-agent/sessions/<encoded-cwd>/<session-id>/
  summary.json            # metadata: title, timestamps, model, message count
  updates.jsonl           # ACP session update stream (conversation + tool calls)
  chat_history.jsonl      # raw chat messages sent to the model
  plan.json               # TODO/task list state
  rewind_points.jsonl     # file snapshots for /rewind undo
  signals.json            # session signals (turn count, token usage)
  feedback.jsonl          # user feedback and ratings
  compaction_checkpoints/ # saved state from auto-compact
  subagents/              # child session directories (when subagents are enabled)
```

`summary.json` is the index entry — it contains the session title, model ID, creation/update timestamps, and parent session reference (for restored sessions). `updates.jsonl` is the authoritative conversation log that drives `/load` and session restore.

### TUI

Sessions persist automatically as you chat. To start fresh:

```
/new
```

The TUI creates a new session each time you launch unless you continue a previous one.

### Headless Mode

Control session behavior with flags:

```bash
# New session each time (default)
echoagent -p "Hello"

# Create or resume a named session
echoagent -p "Remember: X=42" -s my-session
echoagent -p "What is X?" -s my-session

# Resume existing session (errors if not found)
echoagent -p "Continue" -r my-session

# Continue most recent session in current directory
echoagent -p "What were we doing?" -c
```

Session ID is returned in JSON output:

```bash
echoagent -p "Hello" --output-format json | jq -r '.sessionId'
```

### Agent stdio (ACP)

When building with ACP, sessions are managed via protocol methods:

```typescript
// Create new session
const { sessionId } = await connection.request("session/new", {
  cwd: "/path/to/project",
  mcpServers: [],
});

// Load existing session
await connection.request("session/load", {
  sessionId: "existing-session-id",
  cwd: "/path/to/project",
  mcpServers: [],
});
```

The agent persists all session updates automatically. Clients can reconnect and load previous sessions by ID.

---

## File Locations

| Path                  | Description                                         |
| --------------------- | --------------------------------------------------- |
| `~/.echo-agent/config.toml` | Configuration file                                  |
| `~/.echo-agent/sessions/`   | Persisted sessions (organized by working directory) |
| `~/.echo-agent/auth.json`   | Authentication credentials (auto-managed)           |
| `~/.echo-agent/memory/`     | Cross-session memory files and index                |
| `~/.echo-agent/skills/`     | User-scoped skill definitions                       |
| `~/.echo-agent/plugins/`    | User-scoped plugins                                 |
| `~/.echo-agent/agents/`     | User-scoped agent definitions                       |
| `.echo-agent/config.toml`   | Project-scoped config (MCP servers)                 |
| `.echo-agent/skills/`       | Project-scoped skill definitions                    |
| `.echo-agent/plugins/`      | Project-scoped plugins                              |
| `.echo-agent/agents/`       | Project-scoped agent definitions                    |
| `.echo-agent/hooks/`        | Project-scoped hooks                                |
| `.echo-agent/lsp.json`      | LSP server configuration                            |
| `~/.claude/skills/`   | User-scoped skills (Claude Code compat)             |
| `~/.claude/plugins/`  | User-scoped plugins (Claude Code compat)            |
| `~/.claude.json`      | MCP servers (Claude Code compat)                    |
| `.mcp.json`           | Project-scoped MCP servers (Claude Code compat)     |

---

## Environment Variables

| Variable                         | Description                                                                                              |
| -------------------------------- | -------------------------------------------------------------------------------------------------------- |
| `ECHO_AGENT_API_KEY`         | API key from [console.echo-agent.invalid](https://console.echo-agent.invalid). Used for custom endpoint auth and API key login      |
| `ECHO_AGENT_CLI_CHAT_PROXY_BASE_URL`  | Override the cli-chat-proxy URL (default: `https://chat-proxy.echo-agent.invalid/v1`)                          |
| `ECHO_AGENT_MODELS_BASE_URL`          | Custom base URL for inference. Model list auto-fetched from `{base_url}/models` (see [Custom Models Endpoint](#custom-models-endpoint)) |
| `ECHO_AGENT_MODELS_LIST_URL`          | Override the model list URL if it differs from `{ECHO_AGENT_MODELS_BASE_URL}/models`                                              |
| `ECHO_AGENT_AUTH_PROVIDER_COMMAND`     | External auth binary (alternative to config file). See [External Auth Provider](#external-auth-provider) |
| `ECHO_AGENT_AUTH_TOKEN_TTL`            | Token lifetime in seconds for external auth providers that output bare tokens. See [External Auth Provider](#external-auth-provider) |
| `ECHO_AGENT_AUTH_EARLY_INVALIDATION_SECS` | Seconds before `expires_at` to consider a token expired (default: `300`). See [Automatic Credential Refresh](#automatic-credential-refresh) |
| `ECHO_AGENT_OIDC_ISSUER`              | OIDC issuer URL (alternative to config file). See [OIDC](#oidc-customer-sso)                             |
| `ECHO_AGENT_OIDC_CLIENT_ID`           | OIDC client ID (alternative to config file). See [OIDC](#oidc-customer-sso)                              |
| `ECHO_AGENT_HOME`                     | Override config directory (default: `~/.echo-agent`)                                                           |
| `ECHO_AGENT_SUBAGENTS`                | Enable (`1`) or disable (`0`) subagent/task tool support                                                 |
| `ECHO_AGENT_MEMORY`                   | Enable (`1`) or disable (`0`) cross-session memory                                                       |
| `ECHO_AGENT_AGENT`                    | Custom agent definition path or name (see [Agent Profiles](#agent-profiles))                             |
| `ECHO_AGENT_WEB_FETCH`                | Enable (`1`) or disable (`0`) the `web_fetch` tool                                                       |
| `ECHO_AGENT_WEB_FETCH_PROXY`          | Egress proxy URL for `web_fetch` requests (overridden by `[toolset.web_fetch] proxy_endpoint`)           |
| `ECHO_AGENT_RESPECT_GITIGNORE`        | Disable `.gitignore` filtering in tools when set to `0`                                                  |
| `ECHO_AGENT_FEEDBACK_ENABLED`         | Enable (`1`) or disable (`0`) feedback system independently from telemetry                               |
| `ECHO_AGENT_DEPLOYMENT_KEY`           | Management API key for enterprise deployments                                                            |
| `ECHO_AGENT_LOG_FILE`                 | Enable file logging by providing a file path (the value is used verbatim as the path)                    |
| `ECHO_AGENT_DEBUG_LOG`                | Debug firehose (set by `--debug`): truthy routes per-session logs to `~/.echo-agent/debug/<sessionId>.txt`, a path writes that one file |
| `RUST_LOG`                      | Log filter for stderr (headless `-p` defaults to `off`, other non-TUI modes to `error`; TUI captures stderr) and for the `ECHO_AGENT_LOG_FILE` log; the `--debug` firehose ignores it |

---

## Shell Completions

Generate completions for your shell and install them to enable tab completion for `echoagent` commands and flags.

**Note:** The paths below are recommended defaults. Some environments do not automatically source the standard locations — you may need to adapt them to your shell framework or distro conventions.

### Bash

Generate and install:

```bash
mkdir -p ~/.local/share/bash-completion/completions
echoagent completions bash > ~/.local/share/bash-completion/completions/echoagent
```

Reload your shell or run `source ~/.bashrc`.

Alternative (EchoAgent-managed location):

```bash
mkdir -p ~/.echo-agent/completions/bash
echoagent completions bash > ~/.echo-agent/completions/bash/echoagent.bash
```

Add to `~/.bashrc`:

```bash
[[ -r "$HOME/.echo-agent/completions/bash/echoagent.bash" ]] && source "$HOME/.echo-agent/completions/bash/echoagent.bash"
```

### Zsh

Generate and install:

```bash
mkdir -p ~/.zsh/completions
echoagent completions zsh > ~/.zsh/completions/_echo_agent
```

Add to `~/.zshrc`:

```zsh
fpath=(~/.zsh/completions $fpath)
autoload -Uz compinit
compinit
```

Alternative (EchoAgent-managed location):

```bash
mkdir -p ~/.echo-agent/completions/zsh
echoagent completions zsh > ~/.echo-agent/completions/zsh/_echo_agent
```

Add to `~/.zshrc`:

```zsh
fpath=("$HOME/.echo-agent/completions/zsh" $fpath)
autoload -Uz compinit
compinit
```

### After Upgrading

Regenerate completions after upgrading `echoagent` — the script reflects the CLI of the installed version.

---

## Troubleshooting

### Debug logging

Write logs to a file for debugging. The TUI captures stderr, so `RUST_LOG` alone won't produce visible output in production — use `echoagent --debug` or `ECHO_AGENT_LOG_FILE` instead:

```bash
# Per-session debug log (~/.echo-agent/debug/<sessionId>.txt)
echoagent --debug

# Log to a custom path
ECHO_AGENT_LOG_FILE=/tmp/echo-agent-debug.log echoagent

# Tail the most-recently-opened session's log in another terminal (Unix symlink)
tail -f ~/.echo-agent/debug/latest.txt
```

The `--debug` firehose uses a fixed filter (first-party crates at `debug`) and is not narrowed by `RUST_LOG`. A `ECHO_AGENT_LOG_FILE` log defaults to `debug` and honors `RUST_LOG`, so you can set module-level filters for targeted debugging:

```bash
# Debug auth, info for everything else
ECHO_AGENT_LOG_FILE=/tmp/echo-agent-debug.log RUST_LOG="info,echo_agent_runtime::auth=debug" echoagent
```

### Authentication fails

```bash
# Clear credentials and re-login
echoagent login

# Debug auth issues — check the log for "auth:" entries
echoagent --debug-file /tmp/echo-agent-auth.log -p "hello"
grep "auth:" /tmp/echo-agent-auth.log
```

### Model not found

```bash
# List available models
echoagent models

# Check config.toml for typos in [model.*] sections
```

### MCP server not starting

```bash
# Test the server command manually
npx -y @modelcontextprotocol/server-filesystem /path

# Increase startup timeout in config
[mcp_servers.filesystem]
startup_timeout_sec = 30
```

### Command timeout

```toml
# Increase bash timeout in config.toml
[toolset.bash]
timeout_secs = 300.0
```

### Inspecting session data

Session files are plain JSON/JSONL and can be inspected directly:

```bash
# Find sessions for the current directory
ls ~/.echo-agent/sessions/

# Read session metadata
cat ~/.echo-agent/sessions/<encoded-cwd>/<session-id>/summary.json | jq .

# View conversation history
cat ~/.echo-agent/sessions/<encoded-cwd>/<session-id>/updates.jsonl | head -20

# Count turns in a session
wc -l ~/.echo-agent/sessions/<encoded-cwd>/<session-id>/chat_history.jsonl
```

### Context window full

If auto-compact triggers too often, lower the threshold to compact earlier and preserve more headroom:

```toml
[session]
auto_compact_threshold_percent = 70    # default is 85
```

---

## License

Licensed under the Apache License, Version 2.0. See the repository root
`LICENSE` file.
