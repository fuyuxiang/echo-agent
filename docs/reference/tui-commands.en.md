# Terminal Interaction Commands

`echo-agent cli` defaults to the native-scrollback inline renderer;
`echo-agent cli --tui` selects the full-screen Textual renderer. Both share the
commands and WebSocket behavior below. Command names are case-insensitive.

## Command Summary

| Command | Type | Syntax | Description |
|---------|------|--------|-------------|
| `/help` | Local | `/help` | Display available commands |
| `/clear` | Local | `/clear` | Clear the display, not history or audit data |
| `/copy` | Local | `/copy [all]` | Copy the last response or whole conversation |
| `/details` | Local | `/details [section state]` | Control process detail |
| `/save` | Local | `/save [--format md\|txt\|json] [path]` | Export conversation or audit data |
| `/theme` | Local | `/theme [light\|dark]` | Report or switch the palette |
| `/reconnect` | Local | `/reconnect` | Re-establish connection to the gateway |
| `/status` | Local | `/status [event_id]` | Query the durable server-side turn state |
| `/quit` | Local | `/quit` | Exit the client |
| `/approve` | Server | `/approve <id> [session\|always]` | Approve a pending tool execution |
| `/deny` | Server | `/deny <id> [reason]` | Deny a pending tool execution |
| `/approvals` | Server | `/approvals` | List all pending approval requests |
| `/clarify` | Server | `/clarify <id> <answer>` | Answer an agent clarification request |

---

## Local Commands

### /help

Display every local and server-side command. The inline prompt also completes
command names after typing `/`.

```
/help
```

### /clear

Clear the current display and renderer-only indexes.

```
/clear
```

!!! tip
    This does not affect conversation history — the agent still remembers prior messages. Use it to reduce visual clutter.

### /copy

Copy the last assistant response or the entire conversation.

```
/copy        # most recent response
/copy all    # full conversation
```

The inline renderer tries the platform clipboard first (`pbcopy`, `wl-copy`,
`xclip`, or `clip`) and can fall back to capped OSC 52 on a TTY. Failure is
reported explicitly instead of claiming success.

### /details

Report or change how much process information is shown.

```
/details
/details thinking expanded
/details tools collapsed
```

The defaults keep the agent's concrete work observable without expanding raw
payloads:

```
── Process detail ──────────────────────────
Thinking:   collapsed
Tools:      collapsed
Activity:   hidden
────────────────────────────────────────────
```

A tool action is printed when execution starts and gets a compact result on
completion. Parallel results repeat a short operand so their correlation stays
clear. Select `tools lean` for a quieter transcript that suppresses successful
read-only calls; failures always remain visible.

The inline prompt keeps a responsive session bar underneath the input. Wide
terminals show connection/session, model, context occupancy, whole-turn elapsed
time, cumulative cost, and memory count. Medium widths retain model and context
percentage; very narrow terminals keep only connection and timing so the bar
never wraps into the input. The default model and context
limit are seeded from configuration on first paint, then replaced by server
telemetry after actual routing. The spinner owns “what is happening now”; the
bar owns time and controls, avoiding duplicate adjacent status sentences.
Terminal outcomes distinguish completed, failed, interrupted, and disconnected
turns. Approval and clarification waits remain part of whole-turn elapsed time;
`/clear` also clears the settled summary, and `/theme` updates both transcript
and bar colors.

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

Report or switch the light/dark palette.

```
/theme              # report current theme
/theme dark
/theme light
```

Set `ECHO_TUI_THEME=light|dark` for a persistent shell preference.

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

Exit the client. `Ctrl+D` exits immediately. `Ctrl+C` first denies a pending
approval or interrupts the active primary turn; while idle it requires a second
press within two seconds to exit.

```
/quit
```

Keyboard exit: `Ctrl+D`.

---

## Server-Side Commands

These commands are sent to the agent runtime via the gateway. They require an active connection.

### /approve

Approve a tool execution that is waiting for user confirmation. Tools in `ask` approval mode pause before executing and wait for explicit user approval.

```
/approve abc123
/approve abc123 session
```

!!! warning
    Approving a tool execution is irreversible. Review the tool name, arguments, and risk level shown in the approval prompt before confirming.

### /deny

Deny a pending tool execution, optionally providing a reason the agent can use to adjust its approach.

```
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

Answer a clarification request emitted by the agent.

```
/clarify clarify-123 use-the-safe-option
```

Both renderers present an interactive choice UI, so users normally select a
number or enter free text instead of constructing this command manually.

---

## Key Bindings

| Key | Action |
|-----|--------|
| `Enter` | Send message |
| `Esc`, then `Enter` | Insert a newline (inline renderer) |
| `Ctrl+C` | Deny pending approval, stop active turn, or guarded exit |
| `Ctrl+D` | Exit immediately |
| `Up` / `Down` | Navigate input history (inline renderer) |

---

## Workflow Tips

!!! tip "Approval workflow"
    When running with `tools.approval_mode: ask` for sensitive tools, keep `/approvals` handy to see what's queued. You can batch-deny with reasons to guide the agent toward safer alternatives.

!!! tip "Long sessions"
    Use `/save --format json` periodically to checkpoint your conversation. The JSON format preserves full metadata and can be reloaded for analysis.

!!! tip "Multiline input"
    In the inline renderer, press `Esc` followed by `Enter` to insert a newline;
    plain `Enter` submits the buffer.

!!! note "No /undo or /retry"
    The catalog has thirteen commands and none re-runs a turn. Local commands are `/help`, `/clear`, `/copy`, `/details`, `/save`, `/theme`, `/reconnect`, `/status` and `/quit`; server commands are `/approve`, `/deny`, `/approvals` and `/clarify`.

    To redo a turn, send a corrected message — the previous exchange stays in history, so the agent sees both. `/clear` only wipes the screen; the session and its history are untouched.
