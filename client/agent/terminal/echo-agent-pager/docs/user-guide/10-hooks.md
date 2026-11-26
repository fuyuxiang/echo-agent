# Hooks

Hooks let you run a script or send an HTTP request at key moments in a EchoAgent session. Use them to automate tasks, enforce safety checks, log activity, send notifications, and integrate your own tools.

---

## What Are Hooks?

A hook is a shell command or HTTP endpoint that EchoAgent calls when a specific lifecycle event occurs. Hooks can:

- **Block actions**: A `PreToolUse` hook can deny a dangerous command before it runs.
- **Keep the agent working**: A `Stop` hook can block the agent from finishing its turn until a condition holds (e.g. the test suite passes) and feed the reason back to the model.
- **React to events**: A `PostToolUse` hook can log every tool execution to a file.
- **Set up context**: A `SessionStart` hook can export environment variables or run setup scripts.

---

## Common Use Cases

- **Safety guards**: Block commands such as `rm -rf /` before they run.
- **Audit logging**: Record tool use and sessions to a file or external service.
- **Notifications**: Send a message when a task finishes.
- **Auto-formatting**: Run `cargo fmt` or `prettier` after edits.
- **Environment setup**: Export variables at session start.
- **Custom workflows**: Trigger builds, tests, or deployments on specific events.

---

## Quick Start

1. Create the hooks directory:

   ```sh
   mkdir -p ~/.echo-agent/hooks
   ```

2. Create a hook file, e.g. `~/.echo-agent/hooks/session-start.json`:

   ```json
   {
     "hooks": {
       "SessionStart": [
         {
           "hooks": [
             { "type": "command", "command": "echo 'EchoAgent session started in '$(pwd)" }
           ]
         }
       ]
     }
   }
   ```

3. Start (or restart) a EchoAgent session. The hook runs automatically on `SessionStart`.

4. Press `Ctrl+L` on non–VS Code family terminals (or run `/hooks` anywhere — preferred on VS Code family) and check the Hooks tab to confirm it loaded.

---

## Hook Locations

Hooks are discovered from several places (all are merged):

| Scope | Path | Trusted? | Notes |
|-------|------|----------|-------|
| Global | `~/.echo-agent/hooks/*.json` | Always | Personal hooks |
| Global | `~/.claude/settings.json` (and `settings.local.json`) | Always | Claude Code compatibility (configurable) |
| Global | `~/.cursor/hooks.json` | Always | Cursor compatibility (configurable) |
| Project | `<project>/.echo-agent/hooks/*.json` | Requires trust | Per-repo automation |
| Project | `<project>/.claude/settings.json` (and `settings.local.json`) | Requires trust | Claude compatibility (configurable) |
| Project | `<project>/.cursor/hooks.json` | Requires trust | Cursor compatibility (configurable) |
| Config | `~/.echo-agent/config.toml` | Always | Your hooks alongside the rest of your config |
| Config | `managed_config.toml` (`$ECHO_AGENT_HOME` and `/etc/echoagent`) | Always | Organization-distributed hooks (server-synced and on-device) |
| Config | `requirements.toml` (user and system) | Always | Organization-distributed hooks in the requirements layer |
| Plugin | Bundled inside installed plugins | Per-plugin | Shared team hooks |

Config-file hooks live in the same TOML your organization already controls; see [Hooks in Config Files](#hooks-in-config-files) for the format. The compatible vendor hook sources are scanned by default. To disable scanning for a specific vendor, set `[compat.<vendor>] hooks = false` in `~/.echo-agent/config.toml` or the corresponding environment variable. See [Configuration](05-configuration.md#harness-compatibility) for details.

**Trusting a project**: The first time you open a project with hooks, you must trust it before its project hooks will run; until then they are silently skipped. Grant trust by running `/hooks-trust` (or launching with `--trust`); the decision is recorded in the unified folder-trust store (`~/.echo-agent/trusted_folders.toml`), the same gate that governs repo-local MCP/LSP servers. Global hooks in `~/.echo-agent/hooks/` are always trusted and need no entry. This prevents untrusted repos from running arbitrary code.

Because hooks are unified under folder-trust, a `--trust` / `/hooks-trust` grant trusts the whole folder for **MCP, LSP, and hooks** together, and cascades to subdirectories. Conversely, disabling folder-trust (`ECHO_AGENT_FOLDER_TRUST=0` or `[folder_trust] enabled = false`) ungates project hooks along with MCP/LSP.

---

## Hook Events

Events fire at three cadences: once per session (`SessionStart`, `SessionEnd`), once per turn (`UserPromptSubmit`, `Stop`, `StopFailure`), and on every tool call inside the turn (`PreToolUse`, `PostToolUse`, `PostToolUseFailure`).

| Event | When it fires | Blocking? |
|-------|---------------|-----------|
| `SessionStart` | A session starts. Does not fire for a subagent's own session. | No |
| `UserPromptSubmit` | You submit a prompt. | No |
| `PreToolUse` | A tool is about to run. | Yes: can deny |
| `PostToolUse` | A tool completes successfully. | No |
| `PostToolUseFailure` | A tool fails. | No |
| `PermissionDenied` | The permission system denies a tool call. | No |
| `Stop` | An agent turn ends on a genuine completion (an interrupt fires `StopCancelled` instead). | Yes: can block the stop |
| `StopFailure` | A turn ends because of an API error. | No |
| `StopCancelled` | Runs instead of `Stop` when a turn ends without completing: a user interrupt (Ctrl+C / Esc / a client stop), a declined permission prompt, the `--max-turns` limit, or a no-progress bail-out. | No |
| `Notification` | User-attention events (`idle_prompt`, `permission_prompt`, `task_complete`, …). | No |
| `SubagentStart` | A subagent starts. | No |
| `SubagentStop` | A subagent's turn ends (fires once, in the subagent, with stop decision control). | Yes: can block the stop |
| `PreCompact` | Conversation compaction is about to run. | No |
| `PostCompact` | Conversation compaction completes. | No |
| `SessionEnd` | The session ends. Carries `subagentType` for a child session, so a host can tell a child's teardown from its own. | No |

`SubagentEnd` is accepted as an alias for `SubagentStop`. `PreToolUse` can block a tool call, and `Stop`/`SubagentStop` can block the agent from stopping (see [Stop Decision Control](#stop-decision-control)); every other event is passive.

### Cursor Hook Compatibility

EchoAgent accepts Cursor's camelCase hook event names, so `~/.cursor/hooks.json` loads unchanged:

| Cursor event | Maps to |
|---|---|
| `sessionStart`, `sessionEnd` | `SessionStart`, `SessionEnd` |
| `preToolUse`, `postToolUse`, `postToolUseFailure` | `PreToolUse`, `PostToolUse`, `PostToolUseFailure` |
| `beforeShellExecution`, `beforeMCPExecution`, `beforeReadFile` | `PreToolUse` |
| `afterShellExecution`, `afterMCPExecution`, `afterFileEdit` | `PostToolUse` |
| `afterAgentResponse`, `afterAgentThought` | `PostToolUse` |
| `beforeSubmitPrompt` | `UserPromptSubmit` |
| `subagentStart`, `subagentStop` | `SubagentStart`, `SubagentStop` |
| `preCompact`, `stop` | `PreCompact`, `Stop` |

Cursor's per-operation hooks (`beforeShellExecution`, `afterFileEdit`, etc.) map to the generic `PreToolUse`/`PostToolUse` events. The hook script receives the tool name in the JSON input and can filter accordingly, or use the `matcher` field.

---

## The Hook JSON Format

Each `.json` file can define hooks for multiple events:

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

### Key Fields

- **Event name** (top-level key): any event listed in [Hook Events](#hook-events). EchoAgent skips unrecognized event names so a shared Claude or Cursor settings file still loads.
- **matcher** (optional): A regular expression that selects which invocations trigger the hook. What it tests depends on the event: the tool name on tool events (`PreToolUse`, `PostToolUse`, `PostToolUseFailure`, `PermissionDenied`), the notification type on `Notification`, the subagent type on `SubagentStart`/`SubagentStop` (e.g. `explore`), the start source on `SessionStart` (`startup`, `resume`, …), the end reason on `SessionEnd`, the compaction trigger on `PreCompact`/`PostCompact` (`manual` or `auto`), the error type on `StopFailure` (`rate_limit`, `authentication_failed`, `invalid_request`, `server_error`, `max_output_tokens`, or `unknown`), and the reason on `StopCancelled` (`user_interrupt`, `permission_rejected`, `permission_cancelled`, `max_turns`, `no_progress`, or `unknown`). A matcher on `Stop` or `UserPromptSubmit` is ignored with a warning (those events always fire). An empty or omitted matcher matches everything. A finish-thinking chime should set `matcher` to `idle_prompt` on `Notification` (any turn end, then sustained idle); `permission_prompt` fires only when a permission UI is actually waiting. The matcher tests the real tool name; MCP calls routed through the internal `use_tool` dispatcher appear as the qualified `server__tool` name (e.g. `linear__save_issue`), so match on that, not the dispatcher name.
- **type**: `"command"` (run a script or shell one-liner) or `"http"` (POST the event to a URL).
- **command**: Path to executable (relative to the JSON file) or inline shell command.
- **timeout**: Seconds before killing the hook (default: 5, or 600 for `Stop`/`SubagentStop` gates, matching Claude Code). All hook failures (timeouts, crashes, malformed output, missing required env vars) are fail-open: the failure is recorded for the UI scrollback but the tool call is not blocked. Only an explicit `deny` decision returned by the hook blocks a tool call.

### Tool Name Aliases

In a `matcher`, EchoAgent maps Claude-style tool names to its own so hooks migrated from Claude fire correctly. Common aliases include:

- `Bash` → `run_terminal_command`
- `Read` → `read_file`
- `Edit`, `Write`, and `MultiEdit` → `search_replace`
- `Grep` → `grep`
- `Glob` and `ListDir` → `list_dir`
- `WebSearch` → `web_search`
- `Task` → `spawn_subagent`

A matcher keeps its original name too, so `Bash` matches both `Bash` and `run_terminal_command`.

---

## How a Hook Resolves

When an event fires, EchoAgent resolves it in four steps:

1. **Select matching groups.** For that event, each matcher group whose `matcher` matches the event's field runs. The matcher tests the tool name on tool events, the notification type on `Notification`, and so on (see [Key Fields](#key-fields)). An empty or omitted matcher matches everything.
2. **Run the handlers in order.** Handlers in the selected groups run in config order, each receiving the event as JSON on stdin, until one returns `deny` (which stops the chain). Handlers from different sources (global, project, plugin, config) are merged, and identical handlers are deduplicated. Every handler sees the model's original tool input; a `PreToolUse` `updatedInput` is applied only after all handlers finish, so one handler cannot see another's rewrite (the last rewrite wins).
3. **Apply the decision.** For a `PreToolUse` gate, the first `deny` blocks the call and its reason is shown to the model, an `updatedInput` rewrites the tool input, and otherwise the call proceeds. For `Stop` and `SubagentStop`, a `block` keeps the agent working. Every other event is passive: its output is recorded but does not change control flow.
4. **Fail open.** A handler that times out, crashes, or emits malformed output is recorded in the scrollback but never blocks the action. The one exception is a `PreToolUse` `updatedInput` that fails the tool's schema: the rewrite cannot run safely, so the call is blocked and reported as an invalid-input error. Otherwise only an explicit `deny` blocks a tool call.

---

## Hooks in Config Files

Hooks can also live directly in your EchoAgent config, so a team can distribute them with the rest of their configuration instead of shipping separate JSON files. The same `hooks` object is read from three TOML files:

| File | Tier | Who sets it |
|------|------|-------------|
| `~/.echo-agent/config.toml` | User | You |
| `managed_config.toml` (`$ECHO_AGENT_HOME`, `/etc/echoagent`) | Managed / system | Your organization |
| `requirements.toml` (user and system) | Requirements | Your organization |

The TOML is structurally identical to the JSON hook object, so an existing hook transliterates directly:

```toml
[[hooks.PreToolUse]]
matcher = "Bash|Write|Edit"
hooks = [
  { type = "command", command = "/opt/guard/pretooluse.sh", timeout = 10 },
]
```

Each matcher group is a `[[hooks.<Event>]]` entry with an optional `matcher` and an inner `hooks` array of handlers. The handler fields (`type`, `command`, `url`, `timeout`, `env`) and event names are exactly the same as the [JSON format](#the-hook-json-format).

TOML offers two equivalent notations for the inner handlers, and both parse to the identical structure. The inline-table array shown above is recommended: it reads best for the common single-handler case. The nested array-of-tables form is also accepted:

```toml
[[hooks.PreToolUse]]
matcher = "Bash|Write|Edit"
[[hooks.PreToolUse.hooks]]
type = "command"
command = "/opt/guard/pretooluse.sh"
timeout = 10
```

Prefer the inline form to avoid repeating the `[[hooks.<Event>.hooks]]` header for each handler.

- **Additive across layers.** Every layer's hooks run; a lower-priority layer adds hooks but never replaces another layer's block. A hook defined identically in more than one layer is deduplicated, keeping the highest-authority copy.
- **Provenance labels.** Config hooks appear in `/hooks` tagged by origin (`managed:`, `requirements/user:`, `user:`, and so on) so you can see which layer contributed each one.
- **No read-time expansion.** A literal `${VAR}` in a `command` or `url` reaches the hook runner unchanged, matching JSON hook-file semantics; the runner performs the single expansion.

---

## Writing Hook Scripts

### Input

The event is sent as JSON on **stdin** (for example, a `PreToolUse` event; the payload also always includes `toolUseId` and `toolInputTruncated`):

```json
{
  "hookEventName": "pre_tool_use",
  "sessionId": "abc-123",
  "cwd": "/Users/you/project",
  "workspaceRoot": "/Users/you/project",
  "permissionMode": "default",
  "toolName": "run_terminal_command",
  "toolInput": { "command": "npm test" },
  "timestamp": "2026-04-14T12:00:00Z"
}
```

Every event carries the same common fields: `hookEventName`, `sessionId`, `cwd`, `workspaceRoot`, `timestamp`, `permissionMode` (`default`, `auto`, `plan`, or `bypassPermissions`), and `promptId` (the turn the event belongs to; absent for session-scoped events), plus event-specific fields like `toolName` above.

### Output (Blocking Hooks)

For `PreToolUse` hooks, write JSON to **stdout**:

- **Allow**: `{"decision": "allow"}`
- **Deny**: `{"decision": "deny", "reason": "Unsafe command detected"}`
- **Rewrite the tool input**: `{"hookSpecificOutput": {"hookEventName": "PreToolUse", "updatedInput": {"command": "npm test"}}}`

`updatedInput` replaces the tool's input before it runs. The value must be a JSON object; a non-object is ignored. The rewritten input is what the plan-mode gate, the permission prompt, and the tool itself all see, so a hook can normalize or harden a call rather than only allow or deny it. Because hooks run before the plan-mode gate, a hook with side effects fires even when plan mode later rejects the call. If the rewritten input fails the tool's schema, the call is blocked and reported as an invalid-input error rather than falling back to the original. A `deny` decision discards any `updatedInput`; when several hooks return one, the last wins. Omitting `decision` while returning `updatedInput` allows the call and applies the rewrite.

### Exit Codes

| Exit Code | Meaning |
|-----------|---------|
| `0` | Success / allow (for blocking hooks) |
| `2` | Explicit deny (`PreToolUse`) or block-stop with stderr as feedback (`Stop`/`SubagentStop`). For `PreToolUse`, the first stderr line (capped) becomes the deny reason when the JSON carries none; `Stop`/`SubagentStop` feed the full stderr to the model. |
| Other | Fail-open — the failure is recorded (as `exit code N: <first stderr line>`) but nothing is blocked. For `PreToolUse`, a `deny` decision in stdout JSON is honored regardless of exit code. For `Stop`/`SubagentStop`, a valid decision JSON on stdout wins over the exit code; the exit code decides only when stdout has no usable JSON, in which case exit 2 blocks with stderr as the feedback. |

Write human-readable diagnostics to **stderr**: it is the hook's feedback channel. On failures the first stderr line appears in the scrollback entry and logs instead of a bare exit code.

### Stop Decision Control

`Stop` and `SubagentStop` hooks run when the agent is about to finish its turn and can keep it working (Claude Code-compatible). Write JSON to **stdout**:

- **Block the stop**: `{"decision": "block", "reason": "The test suite hasn't been run yet"}`. The reason is fed back to the model as a user message and the agent runs another round in the same turn.
- **Non-error feedback**: `{"hookSpecificOutput": {"hookEventName": "Stop", "additionalContext": "Run the linter before finishing"}}`. Also keeps the agent working, but is surfaced as hook feedback rather than a hook error.
- **Force stop**: `{"continue": false, "stopReason": "Budget exhausted"}`. Ends the turn, overriding any blocks.
- **Allow the stop**: exit 0 with no output (or any non-JSON output).

Exiting with code `2` also blocks the stop, with **stderr** as the feedback.

The hook input includes `stopHookActive` and `lastAssistantMessage`. `stopHookActive` is true when the agent is already continuing due to a previous stop-hook block this turn; check it, or the transcript, to avoid blocking on a condition that will never resolve. `lastAssistantMessage` carries the text of the agent's final response this turn, so hooks can act on it without parsing the transcript. Every event carrying this field clips it at 32,768 characters, with the same `… [+N chars]` marker as the other free-text fields. It is far looser than the 1,000 applied to `errorDetails` and friends because it carries a whole answer rather than a label, and it is sized to the same scale as the tool payload cap. After **8 continuations** (blocks or non-error feedback) in one turn the gate is overridden and the turn ends; hooks are not consulted for that final, forced stop. The counter is per turn: the next user prompt starts fresh, so a long-running goal can span turns. Hook failures fail open: the agent stops normally.

`Stop` and `SubagentStop` hooks default to a 600-second timeout (matching Claude Code) because gates commonly run builds or test suites, and a timed-out hook fails open, so the agent stops anyway. Other events keep the 5-second default. Set `timeout` explicitly when a gate needs more: `{ "type": "command", "command": "bin/verify.sh", "timeout": 1200 }`.

The gate runs only for genuine completions. A turn that was interrupted (Esc / Ctrl+C), refused, or cut off at the turn limit skips the Stop gate, though a Ctrl+C that lands while a Stop hook is already running kills it mid-flight (see below); API-error turns fire `StopFailure`, and cancelled turns fire `StopCancelled`. A separate Stop also fires at session end (`reason: "channel_closed"` or `"shutdown"`); its decision output is parsed but ignored, since there is no turn left to continue. A script that counts or gates on Stop fires should check `reason == "end_turn"` so the session-end fire doesn't skew it.

`StopFailure` is observation-only (use it to log failures or send alerts; output and exit code are ignored). Its input carries `error` (the classified type the matcher tests: `rate_limit`, `authentication_failed`, `invalid_request`, `server_error`, `max_output_tokens`, or `unknown` for anything the runtime cannot distinguish; capacity errors classify as `rate_limit`), `errorDetails` (the raw error detail, when available, clipped at 1000 characters; absent for a refusal, whose explanation rides `lastAssistantMessage` alone), `lastAssistantMessage` (the rendered error text shown in the conversation; for this event it is the error string, not assistant output), and `subagentType` (the subagent's type when the turn ran inside one).

`StopCancelled` is observation-only too. **It runs instead of `Stop` when the turn ends without completing**, the same way `StopFailure` runs instead of `Stop` on an API error.

A turn reports **at most one** of the three, with one exception noted below: a `Stop` hook that ran to completion can still be followed by `StopCancelled` if the user interrupts during the gate, because by then the hook has already been told the turn ended. Every turn that runs the model and then ends, errors, or is cancelled reports one, except for the cases listed below.

If your host must never miss an idle transition, listen for the `idle_prompt` `Notification` as well. It covers every exception in which the session is still alive, with one gap: a session whose only activity was a bash-mode command that ran to completion earns neither the report nor the ping, though interrupting one earns both. `SessionEnd` covers teardown. The `idle_prompt` ping fires about a minute after the session settles, needs at least one turn to have ended, and is cancelled if you send another message first.

A cancelled turn's report is dispatched off the session's command loop, so an interrupt is never delayed by your hook. The report can therefore arrive **after** the next turn's `UserPromptSubmit`, and turn-end reports are not ordered against each other across paths.

A script that tracks busy and idle should key on `promptId`, following the rules below. echoagent mints one per turn, but a client that supplies its own in `_meta` owns its uniqueness, so treat the id as opaque and scope it to the session.

Every turn-end report goes through one worker, so a slow hook delays the next report but never the turn it belongs to. Keep observe-hook timeouts short.

An interrupt while a `Stop` hook is running kills that hook mid-flight, and the turn then reports `StopCancelled`: a `Stop` hook that started is not a promise the turn completed. A `StopFailure` hook runs off the turn, so an interrupt cannot kill it, and that turn has already reported, so no `StopCancelled` follows.

`Stop` is a gate, so when a stop hook blocks it fires again for each continuation round; only the fire that lets the turn end is the report, and a turn that ends cancelled or failed after a blocked `Stop` reports that instead. A passive observer cannot tell a continuation fire from the final one (`stopHookActive` is true for both), so a UI gated on `Stop` alone shows a false idle from the first continuation fire until the user's next prompt, since no `UserPromptSubmit` marks a continuation round. Leave `Stop` out of the state script when you also run a blocking gate, and settle on the `idle_prompt` `Notification` instead.

Some turns report none of the three:

- bash mode (`!`) and builtin slash commands that run to completion. Interrupting one still reports `user_interrupt`, without a preceding `UserPromptSubmit`.
- a cancel-and-send, a rewind, or a queued prompt removed before it ran.
- session teardown, reported by the session-end `Stop` and `SessionEnd`.
- a turn whose stop hooks kept the agent working until the per-turn continuation limit forced the stop.
- a turn where no stop hook ran to completion, because they were all disabled, untrusted, or failed, or, in a subagent, because their matchers all missed. Leaving the turn unreported is deliberate: a later cancel or failure can then report it.
- a turn superseded by the next one while its report was still being built.
- a report still queued when the session exits: teardown waits half a second for queued turn-end hooks, then drops what is left and aborts any hook still running.
- a turn that completed, ran its `Stop`, and only then failed to write to disk: it reported `Stop`, so no `StopFailure` follows. The failure still surfaces in the conversation.

`StopCancelled`'s input carries:

- `reason`: the classified cause, and the value the matcher tests. `user_interrupt` (Ctrl+C, Esc, a client stop button, or a client `session/cancel`), `permission_rejected` (you declined a tool call), `permission_cancelled` (you dismissed the prompt), `max_turns`, `no_progress` (the agent bailed out after repeated no-op rounds), or `unknown` (a cancel the runtime could not classify, and the forward-compatible fallback). The matcher tests this field only, so a hook that wants every user-initiated stop matches the reasons it cares about and reads `cancelledBy` from the payload. New reasons may be added over time, so treat an unrecognized value the way you treat `unknown`.
- `cancelledBy`: `user` for an interrupt, a declined tool call, or a dismissed prompt; `runtime` for everything the agent decided itself, such as `max_turns` and `no_progress`; `unknown` when `reason` is `unknown`, because a cancel the runtime could not classify cannot claim the user was uninvolved. Derived from `reason`, so a new reason classifies automatically. Values may be added here too: treat one you do not recognize the way you treat `unknown`, rather than assuming anything that is not `user` was the runtime.
- `cancelTrigger`: the gesture, when the client named one, clipped at 64 characters, since a gesture name is a token. The bundled pager sends one of four: `ctrl_c`, `esc`, `mouse` (the on-screen stop button), or `dashboard_stop`. Another client may send any string, and it is passed through verbatim. Every value here classifies as `user_interrupt`, including one that happens to spell an internal name such as `shutdown`, because a client asking to cancel is the user asking; read `cancelledBy` from the payload rather than parsing this string. Omitted for a bare `session/cancel` and for every runtime-initiated reason.
- `reasonDetails`: the same kind of detail `StopFailure` puts in `errorDetails`, when the runtime has one. For a declined tool call it is `<tool>: <why>`. Clipped at 1000 characters, like `StopFailure`'s `errorDetails`.
- `lastAssistantMessage`: whatever the turn had committed to the conversation at the interrupt, if any. A Ctrl+C during the final answer leaves the last committed text, or nothing if the turn never committed any. Clipped like the same field on `Stop` and `StopFailure`.
- `subagentType`: the subagent's type when the turn ran inside one, so a hook can tell a nested agent's stop from the session's. Absent in the main session.

The envelope's `timestamp` is stamped when the hook dispatches, not when the turn ended. The three turn-end events queue behind one worker, so a report waiting on a slow hook ahead of it carries a later timestamp than the moment it describes. Use `promptId` to correlate, not the clock.

`StopCancelled` cannot block: the turn is already over, and letting a hook reopen a turn the user deliberately stopped would fight the user. Use `Stop` when you want to keep the agent working.

A "cancel-and-send" (typing a new message while a turn runs) does **not** fire `StopCancelled`, because the turn is being replaced, not stopped, and the agent stays busy. Inside a subagent, a `user_interrupt` does not fire either: it follows the parent's cancel, and the session-level signal is the useful one. A subagent's own `max_turns`, `no_progress`, or declined permission does fire. The matcher tests `reason` only, so a script that reports whether the session is idle should exit early when `subagentType` is present.

A complete busy and idle indicator takes five registrations. `UserPromptSubmit` marks the session busy;
`Stop`, `StopFailure`, and `StopCancelled` settle it however the turn ended; the `idle_prompt`
`Notification` is the backstop for the turns that report none of the three. Registering only
`StopCancelled` leaves the host busy after every normal turn.

```json
{
  "hooks": {
    "UserPromptSubmit": [{ "hooks": [{ "type": "command", "command": "bin/turn-started.sh" }] }],
    "Stop": [{ "hooks": [{ "type": "command", "command": "bin/turn-ended.sh", "timeout": 10 }] }],
    "StopFailure": [{ "hooks": [{ "type": "command", "command": "bin/turn-ended.sh" }] }],
    "StopCancelled": [{ "hooks": [{ "type": "command", "command": "bin/turn-ended.sh" }] }],
    "Notification": [
      { "matcher": "idle_prompt", "hooks": [{ "type": "command", "command": "bin/turn-ended.sh" }] }
    ]
  }
}
```

What the two scripts have to get right:

- **Track the newest `promptId` and ignore reports for older turns.** A cancelled turn's report is
  dispatched off the command loop, so it can arrive after the next turn's `UserPromptSubmit`.
- **Settle unconditionally when there is no `promptId`.** That is echoagent reporting on the session
  rather than a turn: the `idle_prompt` ping and the session-end `Stop`. It is what makes the
  backstop work for a rewind or a superseded turn, which report nothing.
- **Treat a `promptId` you never saw start as idle.** An interrupted bash-mode turn reports without
  a preceding `UserPromptSubmit`.
- **Exit early when `subagentType` is present.** A subagent's stop is not the session's.
- **Settle the host before you record the turn as handled,** so a hook killed mid-flight leaves the
  turn correctable. Re-read that record first, so you only clear a turn you recorded yourself.
- **Keep it to a local write.** Teardown gives the whole queue of turn-end reports half a second,
  and the session's ten-second exit budget bounds the `SessionEnd` hooks after that.

`Stop` is a gate, so that entry runs on the turn's critical path: keep it fast, give it a `timeout`,
and exit 0, because exit 2 blocks the stop and keeps the agent working. Leave `Stop` out if you also
run a blocking `Stop` gate, since a continuation fire would settle the host while the agent is still
going; register `SessionEnd` instead, which is the only thing that settles a session that exits
before the ping.

Both scripts also run inside a subagent's own session. Every event either script reads carries
`subagentType` there and omits it in the main session, so `[ -n "$subagentType" ] && exit 0`
filters a child out of both halves. This matters most for a background subagent, which outlives
the parent turn and would otherwise hold the host busy after the parent went idle.

`Stop` input also carries `backgroundTasks` and `sessionCrons`, so a hook can distinguish "session is done" from "session is paused waiting for background work to wake it back up". Both arrays are empty when nothing is in flight or scheduled. Each `backgroundTasks` entry describes one in-flight task: `id`, `type` (`shell`, `monitor`, or `subagent`), `status`, and (depending on the type) `command` (shell tasks only), `description` (a monitor's watched command line, or a subagent's task description), and `agentType` (subagents). Each `sessionCrons` entry describes one scheduled wakeup (`scheduler_create` or `/loop`): `id`, `schedule`, `recurring`, and `prompt`. The `schedule` value is a human-readable interval such as `every 5 minutes`; echoagent schedules are intervals, not cron expressions. Free-text entry fields are capped at 1000 characters with an in-string `… [+N chars]` marker.

Inside a subagent, the gate fires as `SubagentStop` (agent-frontmatter `Stop` hooks are automatically remapped). A `Stop` hook only gates the main agent.

`SubagentStop` fires once per subagent, at the subagent's own turn end, matching Claude Code. Its input carries a `phase` field (currently always `"gate"`) reserved for forward compatibility.

**Porting Claude Code stop hooks**: the output vocabulary (`decision`, `reason`, `continue`, `stopReason`, `additionalContext`) works unchanged. Check this list for what does not match Claude:

- **camelCase input**: echoagent's stdin envelope uses camelCase keys throughout where Claude uses snake_case. A script reading `.stop_hook_active`, `.hook_event_name`, or `.background_tasks[].agent_type` must switch to `.stopHookActive`, `.hookEventName`, and `.backgroundTasks[].agentType` (the event value is `"stop"`). Hooks registered through the echo-agent-agent-sdk convert both the top-level keys and the `backgroundTasks`/`sessionCrons` entry keys to snake_case, so the wire's `.backgroundTasks[].agentType` reads as `.background_tasks[].agent_type` in the SDK.
- **`toolResult` field**: the `PostToolUse` tool output is `toolResult` (SDK: `tool_result`), not Claude's `tool_response`; a hook reading `.tool_response` must switch to `.toolResult`.
- **Session-end fire**: an extra observe-only Stop fires at session end; filter on `reason == "end_turn"` (see above).
- **Interval schedules**: `sessionCrons[].schedule` is a human-readable interval, never a cron expression.
- **Task types**: `backgroundTasks[].type` is only `shell`, `monitor`, or `subagent`; Claude's other labels (`workflow`, `teammate`, …) are not emitted.
- **StopFailure classes**: echoagent emits six (`rate_limit`, `authentication_failed`, `invalid_request`, `server_error`, `max_output_tokens`, `unknown`). Capacity errors (503/529) classify as `rate_limit`. A matcher on an error class echoagent does not emit never fires.
- **Default timeout**: echoagent defaults observe hooks to 5 seconds, which is shorter than most. Set `timeout` explicitly on an imported hook that does real work.
- **`UserPromptSubmit` is observe-only**: echoagent ignores its exit code and its stdout, so an imported prompt-validation hook silently stops blocking. Use `PreToolUse` to enforce.
- **`StopCancelled` is echo-agent-specific**: a config that uses it is not portable to a runtime with no interrupt hook.
- **`idle_prompt` fires on any turn end**: echoagent fires it after an interrupted or errored turn too, not only a completed one, because it reports a state rather than an outcome. Its `message` is display text and can change between releases, so match on `notificationType` instead.
- **Subagent identity is `subagentType`, not `agent_type`**: echoagent puts it in the payload of the events that can fire inside a subagent, matching its own `SubagentStart`/`SubagentStop`, rather than in the common fields.
- **permission_mode values**: echoagent emits `default`, `auto`, `plan`, or `bypassPermissions`. Claude's `acceptEdits`/`dontAsk` have no echoagent equivalent (echoagent's `auto` is the nearest), so a check like `permission_mode === "acceptEdits"` never matches.
- **Client (SDK) gate timeouts**: SDK `Stop`/`SubagentStop` gates default to 600 seconds like file hooks; `PreToolUse` client gates default to 30 seconds (the interactive hot path). Either can be overridden per matcher group via `timeoutS`, capped at 600.
- **`/goal`**: echoagent's goal loop is a separate feature that runs before the stop gate; it is not a prompt-type Stop hook.

A complete keep-working policy in one script:

```bash
#!/bin/bash
input=$(cat)
# Gate only genuine turn ends, not the session-end observe fire.
if [ "$(echo "$input" | jq -r '.reason')" != "end_turn" ]; then exit 0; fi
if ! bin/verify.sh >/dev/null 2>&1; then
  echo '{"decision": "block", "reason": "verify.sh failed; fix the failures before finishing"}'
fi
```

registered as `{ "type": "command", "command": "bin/stop-gate.sh", "timeout": 300 }` with `timeout` sized for the verify step. The hook fires again after each continuation, and the built-in cap ends the turn after 8; check `stopHookActive` to give up earlier on feedback the agent evidently cannot act on.

### Passive Hooks

For events like `SessionStart` or `PostToolUse`, stdout is ignored. Just exit 0 on success.

### Environment Variables

EchoAgent sets several environment variables on every hook process. These are useful when writing context-aware or plugin-aware hook scripts.

#### Runner-injected variables (always available)

These variables are set by the hook runner for **every** hook:

| Variable              | Description |
|-----------------------|-------------|
| `ECHO_AGENT_HOOK_EVENT`     | The name of the event that triggered the hook (e.g. `pre_tool_use`, `session_start`, `post_tool_use`, `session_end`, `stop`, `notification`). |
| `ECHO_AGENT_HOOK_NAME`      | The configured name of this specific hook (includes the plugin prefix for plugin-provided hooks). |
| `ECHO_AGENT_SESSION_ID`     | The unique identifier of the current EchoAgent session. |
| `ECHO_AGENT_WORKSPACE_ROOT` | Absolute path to the root of the current workspace. |
| `CLAUDE_PROJECT_DIR`  | Absolute path to the workspace root. A Claude Code-compatible alias for `ECHO_AGENT_WORKSPACE_ROOT`, set for every hook. |

These variables are **reserved**. Any values you attempt to set for them via the `env` field in your hook JSON are stripped at load time (a warning is logged), and the runner always injects the real values at spawn time.

#### Plugin hook variables

When a hook originates from a plugin, EchoAgent additionally injects the following variables:

| Variable             | Description |
|----------------------|-------------|
| `ECHO_AGENT_PLUGIN_ROOT`   | Absolute path to the plugin's installed directory. |
| `ECHO_AGENT_PLUGIN_DATA`   | Absolute path to the plugin's writable data directory (for storing plugin state, caches, etc.). |

These values are provided by the plugin system. For the four plugin-related keys (`ECHO_AGENT_PLUGIN_ROOT`, `ECHO_AGENT_PLUGIN_DATA`, and their Claude aliases), the plugin adapter ensures the official plugin values always win over any user-declared values in the hook's `env` map.

#### User-defined environment variables

You can supply additional environment variables for an individual hook handler using the `env` field:

```json
{
  "type": "command",
  "command": "bin/my-hook.sh",
  "env": {
    "MY_SECRET": "value",
    "LOG_LEVEL": "debug"
  }
}
```

These variables are passed through to the hook process, but they cannot override the reserved runner or plugin variables listed above.

#### Using variables in `command` and `url` fields

Both `command` and `url` support `${VAR}` and `$VAR` expansion. On Windows PowerShell, known `$VAR` refs are rewritten to `$env:VAR` so they read the child environment. See the custom-hooks reference for load-time vs runtime expansion, the `env` map lookup order, and parameter-expansion modifiers (e.g. `${VAR:-default}`).

---

## HTTP Hooks

Instead of a local script, call a remote endpoint:

```json
{ "type": "http", "url": "https://hooks.example.com/echo-agent-event", "timeout": 15 }
```

The full event envelope is POSTed as JSON.

---

## Managing Hooks in the TUI

### The Hooks Tab

Press `Ctrl+L` on non–VS Code family terminals to open the Extensions modal (Plugins tab), or run `/hooks` (any terminal; required on VS Code family where `Ctrl+L` is interject) to open it on the Hooks tab. In the **Hooks** tab:

| Key | Action |
|-----|--------|
| `r` | Reload all hooks from disk |
| `a` | Add a custom hook by path |
| `x` | Remove the selected hook source (asks for confirmation; press lowercase `y` to confirm) |
| `Space` | Enable or disable the selected hook |
| `f` | Cycle the status filter (All / Enabled / Disabled) |

Hooks are grouped by source: **Global**, **Project**, **Plugin**, and **Custom**.

Each hook shows:
- **Event** it triggers on
- **Command** or **URL** that runs
- **Timeout** duration
- **Status**: enabled or `[disabled]`

### Slash Commands

```
/hooks-list           # Show hooks loaded in this session
/hooks-trust          # Trust this project for hook execution
/hooks-add <path>     # Add a custom hook file or directory
/hooks-remove <path>  # Remove a custom hook
/hooks-untrust        # Revoke trust for this project
```

In the TUI pager, the individual `/hooks-*` commands do not appear in the slash-command list. The `/hooks` modal covers listing, adding, removing, and enabling or disabling hooks; project trust is managed via `/hooks-trust` (or the modal's Trust action), which writes the unified folder-trust store described above.

### Per-Hook Enable/Disable

Enable or disable an individual hook at runtime by pressing `Space` in the Hooks tab. The change takes effect immediately, without restarting the session.

### Mid-Session Reload

Press `r` in the Hooks tab to reload all hooks from disk. EchoAgent re-reads every hook source, so this picks up changes you made to hook files during the session.

---

## Hook Annotations in Scrollback

When hooks execute, their results appear as annotations in the TUI scrollback. You can see which hooks ran, whether they allowed or denied an action, and any output they produced. These annotations appear only when the plugins UI is enabled (the default).

---

## Example: Safe Shell Guard

Block dangerous shell commands:

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Bash",
        "hooks": [
          { "type": "command", "command": "bin/safe-shell.sh", "timeout": 5 }
        ]
      }
    ]
  }
}
```

Where `bin/safe-shell.sh`:

```bash
#!/bin/sh
INPUT=$(cat)
CMD=$(echo "$INPUT" | jq -r '.toolInput.command // empty')

# Block destructive patterns
if echo "$CMD" | grep -qE '(rm -rf /|mkfs|dd if=|:(){ :|& };:)'; then
  echo '{"decision": "deny", "reason": "Blocked potentially destructive command"}' 
  exit 2
fi

echo '{"decision": "allow"}'
```

---

## Security Notes

- Global hooks (`~/.echo-agent/hooks/`) run with your user permissions; treat them like shell scripts.
- Project hooks require folder trust (`/hooks-trust` or `--trust`, the same gate as repo-local MCP/LSP) to prevent supply-chain attacks from malicious repos.
- HTTP hooks send session data; only use trusted endpoints.

---

## Best Practices

1. **Keep hooks fast**: long-running hooks block the UI. Use background processes (`&`) or async where possible.
2. **Use explicit `deny` to block**: hooks fail-open on any error, so a hook that crashes will not block the tool. To enforce policy, your hook must run to completion and emit `{"decision":"deny","reason":"..."}` on stdout. Always handle errors inside your script so it can return an explicit decision.
3. **Use absolute paths or relative to hook file**: scripts in `bin/` next to the JSON file are portable.
4. **Test with the modal**: press `Ctrl+L` (non–VS Code family) or run `/hooks` to verify hooks are loaded and matching before relying on them.
5. **Version control project hooks**: commit `.echo-agent/hooks/` (but never secrets).

---

## Troubleshooting

- **Hook not running?** Press `Ctrl+L` on non–VS Code family (or run `/hooks` anywhere) to see if it is loaded and matched.
- **Project hooks ignored?** The folder may be untrusted. Run `/hooks-trust` (or relaunch with `--trust`).
- **Script not found?** Check the path is relative to the `.json` file and executable (`chmod +x`).
- **See errors?** Capture logs by launching with `RUST_LOG=debug ECHO_AGENT_LOG_FILE=/tmp/echoagent.log echoagent`, then check `/tmp/echoagent.log`.
