# Hook Examples

Sample hooks for EchoAgent. Copy to `~/.echo-agent/hooks/` to enable globally, or to `<project>/.echo-agent/hooks/` for project-scoped hooks (requires `/hooks-trust`).

## Available Examples

### 1. Safe Shell Guard (`safe-shell.json`)

**Type:** blocking (`PreToolUse`)

Denies obviously destructive shell commands before they execute:
- `rm -rf /`, `sudo rm -rf`, `mkfs`, `dd` to devices, fork bombs

**Install:**
```sh
mkdir -p ~/.echo-agent/hooks/bin
cp examples/hooks/safe-shell.json ~/.echo-agent/hooks/
cp examples/hooks/bin/safe-shell-guard.sh ~/.echo-agent/hooks/bin/
chmod +x ~/.echo-agent/hooks/bin/safe-shell-guard.sh
```

### 2. No Recursive Grep (`no-recursive-grep.json`)

**Type:** blocking (`PreToolUse`)

Denies recursive `grep` invocations in the shell before they execute:
- `grep -r`, `grep -R`, `grep --recursive`, `grep --dereference-recursive`,
  `grep -d recurse`, clustered flags (`grep -rn`, `grep -nri`), and `rgrep`

Recursive grep walks an entire directory tree into memory and can OOM-kill the
agent process on large repos. The system prompt already steers the model away from
this, but a prompt is advisory — this hook makes it a hard, deterministic block.
Point the model at the dedicated search tool (ripgrep-backed) instead.

It is careful to avoid false positives: `ls -R | grep foo` (the `-R` belongs to
`ls`), `grep -e -r file` (`-r` is the pattern), and `grep -- -r file` are all
allowed.

**Install:**
```sh
mkdir -p ~/.echo-agent/hooks/bin
cp examples/hooks/no-recursive-grep.json ~/.echo-agent/hooks/
cp examples/hooks/bin/no-recursive-grep-guard.py ~/.echo-agent/hooks/bin/
chmod +x ~/.echo-agent/hooks/bin/no-recursive-grep-guard.py
```
(Requires `python3` on `PATH`.)

### 3. Session Audit Log (`session-log.json`)

**Type:** passive (`SessionStart` + `SessionEnd`)

Appends session metadata to `~/.echo-agent/session-audit.log` — event, session ID, cwd, timestamp.

**Install:**
```sh
mkdir -p ~/.echo-agent/hooks/bin
cp examples/hooks/session-log.json ~/.echo-agent/hooks/
cp examples/hooks/bin/session-log.sh ~/.echo-agent/hooks/bin/
chmod +x ~/.echo-agent/hooks/bin/session-log.sh
```

### 4. Tool Activity Logger (`tool-logger.json`)

**Type:** passive (`PreToolUse` + `PostToolUse`)

Logs all tool calls to `~/.echo-agent/tool-activity.log` — tool name, event type, effective tool name, backgrounded status.

**Install:**
```sh
mkdir -p ~/.echo-agent/hooks/bin
cp examples/hooks/tool-logger.json ~/.echo-agent/hooks/
cp examples/hooks/bin/tool-logger.sh ~/.echo-agent/hooks/bin/
chmod +x ~/.echo-agent/hooks/bin/tool-logger.sh
```

### 5. Stop Gate: verify before finishing (`stop-verify.json`)

**Type:** blocking (`Stop`)

Keeps the agent working until `cargo build` passes. A `Stop` hook runs when the agent is about to finish its turn; returning `{"decision":"block","reason":"…"}` feeds the reason back to the model and runs another round. The built-in cap ends the turn after 8 continuations. The hook sets a 300-second timeout because a timed-out Stop hook fails open and lets the agent stop.

**Install:**
```sh
mkdir -p ~/.echo-agent/hooks/bin
cp examples/hooks/stop-verify.json ~/.echo-agent/hooks/
cp examples/hooks/bin/stop-verify.sh ~/.echo-agent/hooks/bin/
chmod +x ~/.echo-agent/hooks/bin/stop-verify.sh
```

## Format

Hook files use the Claude-compatible JSON format:

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Bash",
        "hooks": [
          { "type": "command", "command": "bin/check.sh", "timeout": 5 }
        ]
      }
    ]
  }
}
```

- **Event names:** `SessionStart`, `PreToolUse`, `PostToolUse`, `Stop`, `SubagentStop`, `SessionEnd` (see the [user guide](../../../terminal/echo-agent-pager/docs/user-guide/10-hooks.md) for the full set)
- **Matcher:** regex on tool name. Claude names like `Bash`, `Read`, `Edit` are auto-expanded to also match EchoAgent names (`run_terminal_cmd`, `read_file`, `search_replace`)
- **Timeout:** in seconds (default: 5)
- **Command:** path to script (relative to hook file directory) or inline shell command

## Script Contract

Scripts receive the hook event envelope as JSON on **stdin** and should write a response to **stdout**:

**For tool gates (`PreToolUse`):**
```json
{"decision":"allow"}
```
or
```json
{"decision":"deny","reason":"Explanation for the user"}
```

**For stop gates (`Stop` / `SubagentStop`):** keep the agent working or force it to stop:
```json
{"decision":"block","reason":"Feedback fed back to the model"}
```
```json
{"hookSpecificOutput":{"hookEventName":"Stop","additionalContext":"Non-error feedback"}}
```
```json
{"continue":false,"stopReason":"Shown to the user; overrides any block"}
```
The turn ends after 8 consecutive continuations. The input carries `stopHookActive` (true once a block has already continued this turn) so a hook can give up.

**Exit codes:** `0` = allow / no decision, `2` = deny (`PreToolUse`) or block-stop with stderr as the feedback, other = fail-open. Valid decision JSON on stdout wins over the exit code.

**For passive hooks:** stdout is informational only. Exit `0` for success.

## Uninstall

Remove the JSON file from `~/.echo-agent/hooks/`. The hook stops running on the next session.