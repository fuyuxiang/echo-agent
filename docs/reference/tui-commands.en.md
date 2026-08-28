# TUI Commands Reference

The Echo Agent TUI (Terminal User Interface) is an interactive chat session launched via `echo-agent cli` or by connecting to a running gateway. Commands are prefixed with `/` and split into two categories: **local** commands processed entirely by the client, and **server-side** commands forwarded to the agent runtime.

## Command Summary

| Command | Type | Syntax | Description |
|---------|------|--------|-------------|
| `/help` | Local | `/help` | Display available commands and key bindings |
| `/clear` | Local | `/clear` | Clear the terminal screen |
| `/copy` | Local | `/copy [n]` | Copy the last (or nth) assistant response to clipboard |
| `/details` | Local | `/details [n]` | Show metadata for the last (or nth) message |
| `/save` | Local | `/save [path]` | Export conversation to a file |
| `/theme` | Local | `/theme [name]` | Switch or list available UI themes |
| `/reconnect` | Local | `/reconnect` | Re-establish connection to the gateway |
| `/status` | Local | `/status [event_id]` | Query the durable server-side turn state |
| `/quit` | Local | `/quit` | Exit the TUI session |
| `/approve` | Server | `/approve [id]` | Approve a pending tool execution |
| `/deny` | Server | `/deny [id] [reason]` | Deny a pending tool execution |
| `/approvals` | Server | `/approvals` | List all pending approval requests |
| `/clarify` | Server | `/clarify [question]` | Ask the agent to clarify its last action |

---

## Local Commands

### /help

Display the command reference and active key bindings.

```
/help
```

Output includes all available commands, their syntax, and the current key binding map.

### /clear

Clear the scrollback buffer and reset the display.

```
/clear
```

!!! tip
    This does not affect conversation history — the agent still remembers prior messages. Use it to reduce visual clutter.

### /copy

Copy an assistant response to the system clipboard.

```
/copy        # copy the most recent response
/copy 3      # copy the 3rd response in the conversation
```

The copied content is plain text with markdown formatting stripped. On Linux, this requires `xclip` or `xsel`; on macOS and Windows it works natively.

### /details

Show metadata for a message: token counts, model used, latency, estimated cost, and tool calls.

```
/details         # last message
/details 5       # message at position 5
```

Example output:

```
── Message #12 ──────────────────────────────
Role:       assistant
Model:      claude-sonnet-4-20250514
Tokens:     prompt=1842  completion=637
Latency:    2.3s (first token: 0.4s)
Cost:       $0.0089
Tools:      filesystem(read), shell(ls)
────────────────────────────────────────────
```

### /save

Export the conversation to a file. Defaults to `echo-<timestamp>.md` under the configured transcript directory (`<workspace>/transcripts` for the CLI).

```
/save                           # default path
/save ~/notes/session.md        # explicit path
/save --format json             # JSON export (includes metadata)
```

| Flag | Description |
|------|-------------|
| `--format md` | Markdown (default) |
| `--format json` | Full JSON with metadata, tokens, tool calls |
| `--format txt` | Plain text, no formatting |

JSON is an audit export: it includes cognitive/tool frames even when hidden by
`/details`, plus authoritative turn-status observations. It survives a local
`/clear`. Credential-shaped fields, bearer tokens, secret URL parameters, and
command-line secret flags are redacted before entering the audit buffer.

### /theme

Switch the TUI color theme or list available themes.

```
/theme              # list available themes
/theme dark         # switch to dark theme
/theme light        # switch to light theme
/theme monokai      # switch to monokai theme
```

Available built-in themes: `dark`, `light`, `monokai`, `solarized`, `nord`.

!!! tip
    Set a permanent default in your config file under `ui.locale` (only `locale` is exposed; theme is not persisted across restarts).

### /reconnect

Re-establish the WebSocket connection to the gateway. Useful after network interruptions or gateway restarts.

```
/reconnect
```

The TUI makes a fresh connection using the same session key. On success it reconciles the latest durable turn and replays a missed final reply when necessary.

### /status

Query the gateway's durable lifecycle record rather than inferring completion
from whether the terminal is still animating.

```
/status                 # latest primary turn in this session
/status 6f8c2a1d        # a specific inbound event id
```

States distinguish accepted/running work, approval or clarification waits,
clean completion, incomplete (including output truncation), failure, and user
interruption. The TUI also performs this reconciliation after reconnecting.

### /quit

Exit the TUI session. Active tool executions are not cancelled — the agent continues running on the gateway.

```
/quit
```

Aliases: `Ctrl+D`, `/exit`, `/q`

---

## Server-Side Commands

These commands are sent to the agent runtime via the gateway. They require an active connection.

### /approve

Approve a tool execution that is waiting for user confirmation. Tools in `ask` approval mode pause before executing and wait for explicit user approval.

```
/approve              # approve the most recent pending request
/approve abc123       # approve a specific request by ID
```

!!! warning
    Approving a tool execution is irreversible. Review the tool name, arguments, and risk level shown in the approval prompt before confirming.

### /deny

Deny a pending tool execution, optionally providing a reason the agent can use to adjust its approach.

```
/deny                               # deny the most recent request
/deny abc123 "use read instead"     # deny with guidance
```

When a reason is provided, the agent receives it as context and may choose an alternative approach.

### /approvals

List all currently pending approval requests with their IDs, tool names, and timestamps.

```
/approvals
```

Example output:

```
Pending approvals (2):
  [abc123] shell("rm -rf /tmp/build")      2m ago
  [def456] filesystem(write, "config.yml") 30s ago
```

### /clarify

Ask the agent to explain or elaborate on its most recent action or reasoning.

```
/clarify                            # generic "explain what you just did"
/clarify why did you choose grep?   # specific question
```

The agent responds with an explanation without advancing the task.

---

## Key Bindings

| Key | Action |
|-----|--------|
| `Enter` | Send message |
| `Shift+Enter` | Insert newline (multiline input) |
| `Ctrl+C` | Cancel current input / interrupt streaming |
| `Ctrl+D` | Quit the TUI |
| `Ctrl+L` | Clear screen (same as `/clear`) |
| `Up` / `Down` | Navigate input history |
| `Ctrl+Up` / `Ctrl+Down` | Scroll output buffer |
| `Tab` | Autocomplete command or file path |
| `Ctrl+R` | Search input history |
| `Esc` | Dismiss autocomplete / cancel selection |

---

## Workflow Tips

!!! tip "Approval workflow"
    When running with `tools.approval_mode: ask` for sensitive tools, keep `/approvals` handy to see what's queued. You can batch-deny with reasons to guide the agent toward safer alternatives.

!!! tip "Long sessions"
    Use `/save --format json` periodically to checkpoint your conversation. The JSON format preserves full metadata and can be reloaded for analysis.

!!! tip "Multiline input"
    For pasting code blocks or multi-paragraph prompts, use `Shift+Enter` to insert newlines. The message is sent only when you press `Enter` on a line that isn't preceded by `Shift`.

!!! note "No /undo or /retry"
    The catalog has thirteen commands and none re-runs a turn. Local commands are `/help`, `/clear`, `/copy`, `/details`, `/save`, `/theme`, `/reconnect`, `/status` and `/quit`; server commands are `/approve`, `/deny`, `/approvals` and `/clarify`.

    To redo a turn, send a corrected message — the previous exchange stays in history, so the agent sees both. `/clear` only wipes the screen; the session and its history are untouched.
