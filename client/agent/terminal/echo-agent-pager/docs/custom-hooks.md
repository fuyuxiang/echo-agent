# Custom Hooks Guide

Hooks let you run custom scripts or HTTP requests at key moments during a EchoAgent session — for example, before or after a tool runs, when a session starts or ends, or when the agent sends a notification.

They are perfect for automation, safety checks, logging, notifications, and integrating with your own tools.

## Why Use Hooks?

Common use cases:

- **Safety guards**: Block dangerous commands like `rm -rf /` before they execute.
- **Audit logging**: Record every tool use or session to a file or external service.
- **Notifications**: Send a Slack/Discord message when a long-running task finishes.
- **Auto-formatting**: Run `cargo fmt` or `prettier` automatically after edits.
- **Environment setup**: Export secrets or set variables at session start.
- **Custom workflows**: Trigger builds, tests, or deployments on specific events.

## Quick Start

1. Create the hooks directory:
   ```sh
   mkdir -p ~/.echo-agent/hooks
   ```

2. Create a simple hook file, e.g. `~/.echo-agent/hooks/session-start.json`:
   ```json
   {
     "hooks": {
       "SessionStart": [
         {
           "hooks": [
            { "type": "command", "command": "echo \"🚀 EchoAgent session started in $(pwd)\"" }
           ]
         }
       ]
     }
   }
   ```

3. Start (or restart) a EchoAgent session. The hook runs automatically on `SessionStart`.

   Try it: press `Ctrl+L` on non–VS Code family (or run `/hooks` anywhere — preferred on VS Code / Cursor / Windsurf / Zed) and check the Hooks tab to confirm it's loaded.

## Hook Locations

Hooks are discovered from several places (all are merged):

| Scope     | Path                              | Trusted?     | Notes |
|-----------|-----------------------------------|--------------|-------|
| Global    | `~/.echo-agent/hooks/*.json`            | Always       | Best for personal hooks |
| Global    | `~/.claude/settings.json`         | Always       | Claude Code compatibility |
| Project   | `<project>/.echo-agent/hooks/*.json`    | Requires trust | Per-repo automation |
| Project   | `<project>/.claude/settings.json` | Requires trust | Claude compatibility |
| Config    | `config.toml`, `managed_config.toml`, `requirements.toml` | Always | Hooks shipped in your (or your organization's) config |
| Plugin    | Bundled inside installed plugins  | Per-plugin   | Shared team hooks |

Config-file hooks use the same schema in TOML form; see the [Hooks user guide](user-guide/10-hooks.md#hooks-in-config-files) for details.

**Trusting a project**: Open the hooks modal (`Ctrl+L` on non–VS Code family, or `/hooks` on any terminal including VS Code family) or run `/hooks-trust` (the same folder-trust gate as `--trust`, recorded in `~/.echo-agent/trusted_folders.toml`) the first time you open a project with hooks. This prevents untrusted repos from running arbitrary code.

## The Hook JSON Format

Each `.json` file can define multiple hooks:

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Bash",
        "hooks": [
          { "type": "command", "command": "bin/safety-check.sh", "timeout": 10 }
        ]
      }
    ],
    "PostToolUse": [
      {
        "hooks": [
          { "type": "command", "command": "bin/log-activity.sh" }
        ]
      }
    ]
  }
}
```

Key fields:

- **Event name** (top-level key): `SessionStart`, `UserPromptSubmit`, `PreToolUse`, `PostToolUse`, `Stop`, `Notification`, `SessionEnd`, etc.
- **matcher** (optional): Regex tested against the event's match value — the tool name on tool events, and per-event values elsewhere (see the user guide's Hooks chapter). Empty = match everything.
- **type**: `"command"` (run a script or shell one-liner) or `"http"` (POST the event to a URL).
- **command**: Path to executable (relative to the JSON file) or inline shell command.
- **timeout**: Seconds before killing the hook (default: 5, or 600 for `Stop`/`SubagentStop` gates). Hooks fail open on timeout.

**Tool name aliases**: Claude-style names like `Bash`, `Edit`, `Read` automatically match EchoAgent's internal names (`run_terminal_cmd`, `search_replace`, `read_file`).

## Writing Hook Scripts

### Input
The full event is sent as JSON on **stdin**. Example for a `PreToolUse` hook:

```json
{
  "hookEventName": "pre_tool_use",
  "sessionId": "abc-123",
  "cwd": "/Users/you/project",
  "workspaceRoot": "/Users/you/project",
  "toolName": "run_terminal_cmd",
  "toolInput": { "command": "npm test" },
  "timestamp": "2026-04-14T12:00:00Z"
}
```

### Output (for blocking hooks like PreToolUse)
Write JSON to **stdout**:

- Allow: `{"decision": "allow"}`
- Deny: `{"decision": "deny", "reason": "Unsafe command detected"}`

**Exit codes** (behavior differs by hook type):
- `0` — success / allow (for blocking hooks)
- `2` — explicit deny (`PreToolUse`) or block-stop with stderr as feedback (`Stop`/`SubagentStop`; see Stop Decision Control in the user guide)
- Any other (including timeout/crash/missing env var) — **fail-open**: the failure is logged and shown in the hook scrollback, but the tool call is not blocked. To block a tool call, return JSON `{"decision":"deny","reason":"..."}` on stdout.

### Passive hooks
For events like `SessionStart` or `PostToolUse`, stdout is ignored. Just exit 0 on success.

### Useful Environment Variables

EchoAgent injects the following variables into every hook process:

- `ECHO_AGENT_HOOK_EVENT` — the event name (e.g. `pre_tool_use`, `session_start`, `post_tool_use`)
- `ECHO_AGENT_HOOK_NAME` — the full configured name of this hook
- `ECHO_AGENT_SESSION_ID` — the current session identifier
- `ECHO_AGENT_WORKSPACE_ROOT` — absolute path to the workspace root

For hooks provided by plugins, the following are also set:

- `ECHO_AGENT_PLUGIN_ROOT` — absolute path to the plugin's installation directory
- `ECHO_AGENT_PLUGIN_DATA` — absolute path to the plugin's writable data directory

These runner- and plugin-injected variables always take precedence. Attempts to override the reserved runner keys via the `env` field are stripped at load time (with a warning logged). For plugin hooks, `ECHO_AGENT_PLUGIN_ROOT` and `ECHO_AGENT_PLUGIN_DATA` similarly override any user-supplied values for those keys.

### Custom Environment Variables (`env` field)

Each handler can declare additional env vars to inject into the child process:

```json
{
  "type": "command",
  "command": "bin/check.sh",
  "env": {
    "MY_API_TOKEN": "secret-here",
    "LOG_LEVEL": "debug"
  }
}
```

Values must be **strings** — JSON numbers and bools currently fail to parse
(wrap them in quotes if you need them).

For plugin hooks, the plugin adapter additionally injects
`ECHO_AGENT_PLUGIN_ROOT` and `ECHO_AGENT_PLUGIN_DATA`. These keys override any user-declared
values for the same names (the plugin contract is non-negotiable).

### Variable Substitution

`command` and `url` strings support `$VAR` and `${VAR}` substitution at
config-load time:

```json
{
  "type": "command",
  "command": "${HOME}/.config/echo-agent-hooks/check.sh"
}
```

Lookup order for each reference:
1. The handler's own `env` map.
2. The current process environment (the env EchoAgent itself sees).

If a reference is unset in both, it's **preserved verbatim** (e.g. `${UNSET}`
stays as the literal string). Runner-injected names (`CLAUDE_PROJECT_DIR`,
`ECHO_AGENT_WORKSPACE_ROOT`, `ECHO_AGENT_HOOK_EVENT`, `ECHO_AGENT_HOOK_NAME`,
`ECHO_AGENT_SESSION_ID`) are not taken from the EchoAgent process environment at
load. Unix `sh -c` expands them from the child env; Windows PowerShell
rewrites `$VAR` to `$env:VAR`. HTTP `url` substitutes them at request
time. Remaining unresolved command refs are refused with "required env
var(s) not set".

For HTTP hooks specifically, `url` is also re-expanded **at request time**
(immediately before SSRF validation), so plugin-injected vars like
`${ECHO_AGENT_PLUGIN_ROOT}/check` resolve against the plugin's actual path.

#### Parameter-expansion modifiers

POSIX parameter-expansion forms — `${VAR:-default}`, `${VAR-default}`,
`${VAR:=x}`, `${VAR:?msg}`, `${VAR:+x}`, `${VAR%pat}`, `${VAR#pat}`,
`${VAR/pat/repl}`, `${VAR:N:M}` — are **never** expanded at load time and are
left verbatim for the runtime `sh -c` branch to handle. This avoids subtle
divergences between the load-time expander and POSIX shell semantics
(notably, the empty-string behaviour of `:-`).

If your hook command contains shell metacharacters (spaces, pipes, `&&`,
redirects, `$`, etc.), the runner routes it through `sh -c` and you get full
shell-expansion semantics. If your command is a bare path with no metachars,
the runner spawns it directly — but `$VAR` / `${VAR}` references in the path
are still resolved at load time so direct-exec paths like
`${HOME}/bin/check.sh` work without needing to be wrapped in `sh -c`.

#### What is NOT expanded

- **`matcher`** is a regex (`$` is the regex anchor for end-of-line). It is
  never env-expanded — substituting `$VAR` would silently change the regex's
  semantics and likely produce an invalid pattern. If you need a dynamic
  matcher, generate the JSON file at write time.
- **`timeout`** is numeric, so there is nothing to expand.
- **The values of the `env` map itself** — these are stored verbatim and
  passed to the child as-is, so `"BAR": "${HOME}/x"` injects the literal
  string `${HOME}/x` into the child's environment.

## Managing Hooks in the TUI

Press `Ctrl+L` on non–VS Code family (or run `/hooks` anywhere) to open the Hooks & Plugins modal.

In the **Hooks** tab you can:
- `l` — Reload all hooks
- `a` — Add a custom hook by path (great for testing)
- `e` — Enable/disable
- `r` — Remove
- `Space` — Expand groups

Hooks from `~/.echo-agent/hooks/` appear under **Global**, project ones under **Project**, etc.

## HTTP Hooks

Instead of a local script, call a remote endpoint:

```json
{ "type": "http", "url": "https://hooks.example.com/echo-agent-event", "timeout": 15 }
```

The full event envelope is POSTed as JSON. Useful for webhooks, analytics, or serverless functions.

## Best Practices

1. **Keep hooks fast** — long-running hooks block the UI (use background `&` or async where possible).
2. **Use explicit `deny` to block** — hooks fail-open on any error (timeout, crash, missing env var, etc.), so a hook that crashes will not block the tool call. To enforce policy, your hook must run to completion and emit `{"decision":"deny","reason":"..."}` on stdout.
3. **Use absolute paths or relative to hook file** — scripts in `bin/` next to the JSON are portable.
4. **Test with `Ctrl+L` (non–VS Code family) / `/hooks`** — verify loading and matching before relying on them.
5. **Version control project hooks** — commit `.echo-agent/hooks/` (but never secrets).

## Security Notes

- Global hooks (`~/.echo-agent/...`) run with your user permissions — treat them like shell scripts.
- Project hooks require explicit trust (run `/hooks-trust` or use the modal) to prevent supply-chain attacks from malicious repos.
- HTTP hooks send session data — only use trusted endpoints.

## Troubleshooting

- **Hook not running?** → Press `Ctrl+L` on non–VS Code family (or run `/hooks` anywhere) to see if it's loaded and matched.
- **Project hooks ignored?** → Trust the project first.
- **Script not found?** → Check the path is relative to the `.json` file and executable (`chmod +x`).
- **`The argument '/.claude/hooks/….ps1' to the -File parameter does not exist`?** → PowerShell treated `$CLAUDE_PROJECT_DIR` as empty. EchoAgent rewrites it to `$env:CLAUDE_PROJECT_DIR` unless `ECHO_AGENT_SHELL=cmd`.
- **See errors?** → Check the pager logs (usually in the tracing pane or `~/.echo-agent/logs`).

## More Examples

See the built-in examples in the `echo-agent-hooks` crate:

- [Safe Shell Guard](../../../crates/echo-agent-hooks/examples/hooks/safe-shell.json)
- [No Recursive Grep](../../../crates/echo-agent-hooks/examples/hooks/no-recursive-grep.json) — hard-blocks `grep -r`/`grep -R`/`rgrep` (OOM guard)
- [Session Audit Log](../../../crates/echo-agent-hooks/examples/hooks/session-log.json)
- [Tool Activity Logger](../../../crates/echo-agent-hooks/examples/hooks/tool-logger.json)

Copy them to `~/.echo-agent/hooks/` and customize.

## Full Reference

For the complete event list, matcher semantics, trust model, and advanced details, see the [Hooks user guide](user-guide/10-hooks.md).

---

*Happy hooking!* If you build something cool, consider sharing it as a plugin.