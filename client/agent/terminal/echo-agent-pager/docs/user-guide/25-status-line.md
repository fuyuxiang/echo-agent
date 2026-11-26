# Status Line

An optional row at the bottom of the pager — above the shortcuts bar in the full screen, under the prompt's info row in minimal mode — and disabled by default. It shows live session context, such as the model, context-window usage, cost, directory, and git worktree, or the output of any script you configure. Opt in with `[ui.status_line]` in `~/.echo-agent/config.toml`.

## Set up

### Built-in

```toml
[ui.status_line]
type = "builtin"
items = ["cwd", "model", "context"]   # default when omitted
```

This renders, for example, `echo-agent-shell-status-line │ EchoAgent 4.5 │ 12% ctx`. Items appear in the order you list them, and long ones are elided with `…`: the directory and session name at 40 columns, the model at 30.

| Item | Shows |
| --- | --- |
| `cwd` | Current directory (basename) |
| `model` | Model display name |
| `context` | Context-window percent, amber at the auto-compaction threshold or at 80% when the agent reports none |
| `cost` | Session cost, hidden below $0.005 so it never shows a misleading `$0.00` |
| `turn-timer` | Elapsed time of the running turn, from one second in |
| `session-name` | Session name, when set |

### Command

Point `command` at a script. EchoAgent pipes [JSON](#available-data) to it on stdin and shows its stdout. A `~/` prefix expands to your home directory.

```toml
[ui.status_line]
type = "command"
command = "~/.echo-agent/statusline.sh"
```

Field names and nesting follow the common status line convention, so a ported script usually needs a small edit rather than a rewrite. Anything the table below does not list is not sent.

### Disabled

`type = "disabled"`, the default, shows nothing; `off`, `none`, and `hidden` are accepted as spellings of `disabled`.

### Options

| Key | Type | Default | Description |
| --- | --- | --- | --- |
| `type` | string | `disabled` | `builtin`, `command`, or `disabled`. |
| `items` | array | `["cwd", "model", "context"]` | Built-in segments, in order. |
| `command` | string | none | Script for `type = "command"`. |
| `padding` | integer | `0` | Horizontal spacing, in characters per side, capped at 16. A padding wide enough to leave no columns reserves the row but paints nothing in it. |
| `refresh_interval` | integer | unset | `command` rows only, in seconds, 1 to 86,400. Re-runs the script this often even when nothing changed, so an idle session can still surface a change — an incident page, a CI status. Unset keeps the row event-driven. The run it schedules carries `"trigger": "refresh_interval"`, and its failures keep the last output rather than painting an error (see [Refresh runs](#refresh-runs)). A script that calls a network should prefer a longer interval and read a cache on `state` runs. |

## How it works

- **Refresh.** The row updates when the session state changes (session start, turn end, a model or effort switch, a HEAD move, a compaction, a client attaching) and continuously while a turn runs, not on a timer. An idle session does not re-run your script, so a clock in it will not tick on its own — unless you set `refresh_interval`, which adds a timer on top of all of the above. These updates are debounced at a fixed 300 ms, so a busy turn cannot run your script every frame; a change that must show at once (a resize, a new snapshot, switching agents) waits only 100 ms. A run already going is never cancelled: the next change waits for it to finish. EchoAgent reads `[ui.status_line]` at startup, so changes to it take effect at the next launch.
- **Output.** Each line you print becomes one line of the row, up to five, and each is cut at 1024 characters, counting the ANSI escapes themselves, so a heavily coloured row has less room for text. A short terminal takes fewer, dropping the surplus from the bottom. ANSI colors are honored; every other escape (cursor motion, line erase, carriage-return overwrite) is dropped. OSC 8 hyperlinks are honored for `http`, `https` and `mailto` targets, and any other target renders as plain text. Stdout past 64 KiB is truncated and the script is stopped. A script that succeeds and prints nothing takes the row away rather than falling back to the built-in segments, so a script that prints only sometimes moves the transcript by a line as it comes and goes.
- **Sizing.** EchoAgent sets `COLUMNS` and `LINES` to the row your output fills, not to the window: the pane's padding and your `padding` are already deducted. `tput` reports these too, since it reads them when stdout is not a terminal. `LINES` is what the row currently fills rather than what it may grow to, so it reads `1` until you print more; the ceiling is five whatever it says. Before the row has painted once, and on a frame with no room for it, the size is the last one the row painted at, or 80x1 if it never has.
- **Shell.** `command` is a shell command line, so `jq -r '…'` and pipes work as written; a path is run directly when it names an executable, and through `sh -c` otherwise, which is what runs a script whose `#!` line is missing or wrong. Quote a path containing spaces as you would at a prompt. Each run is a fresh process, so an edit to the script file applies on the next run.
- **Background work does not survive.** Whatever a script leaves running is killed when the run ends, on every path: a clean exit, a timeout, or too much output. The run ends when your script exits, so anything a background job prints after that is lost.
- **Environment.** Scripts run in the session's working directory, then the repository root, then the pager's own, whichever is a local path first, with a 10 second timeout, after which the row shows `[status line: timed out]`. `COLUMNS` and `LINES` describe the row the script fills, not the window. No shell rc files run (`BASH_ENV` and `ENV` are cleared), and `GIT_OPTIONAL_LOCKS=0`. Pagers and editors are neutralized the same way the rest of EchoAgent neutralizes them, so a `git` or `gh` call inside your script will not block waiting for one.
- **Input.** The JSON payload is written to stdin with a trailing newline, so `read -r line` and `input=$(cat)` both work.

## Refresh runs

Set `refresh_interval` on a `command` row and the script also re-runs on a timer, so an incident page or a CI status can reach the row while the session sits idle:

```toml
[ui.status_line]
type = "command"
command = "~/.echo-agent/statusline.sh"
refresh_interval = 300   # seconds
```

- **The payload says why the script ran.** A run that answers the timer carries `"trigger": "refresh_interval"` — a state change landing while a timer fire is owed rides that run — and a run with no fire owed carries `"trigger": "state"`. Hit the network on `refresh_interval` and read a cache on `state`, or a busy turn — which re-runs the script continuously — becomes a request storm against whatever the script calls.
- **The payload is the last one EchoAgent sent.** A timer run re-runs your script with the payload from the last state change, so its session numbers — cost, context, tokens — are as of that change, not of the fire. Only what your script fetches itself is fresh.
- **Refresh failures keep the last output.** Once your script has answered — printed a row, or deliberately nothing — a timer run that fails or times out leaves the row exactly as it was, whether that is the last output or a failure a state run had already painted, and writes the failure to `~/.echo-agent/logs/unified.jsonl`, so a flaky endpoint does not paint an error over a quiet night. Three consecutive refresh failures mean the script itself is broken, and the error shows after all; a refresh failure before the script has answered anything — a fresh session, or right after switching agents — also paints at once, since there is nothing to keep. A run triggered by session state still reports its failure at once, as ever.
- **Missed fires coalesce.** While the row is hidden (a fullscreen subagent view, the welcome screen) or a run already holds the slot, the fire waits and the row is owed one run when it can have it — never a burst for the fires a suspend or a long turn skipped. The timer keeps its cadence whatever your script's runtime: a fire that comes due while a run is still going is carried to the next run rather than stacked behind it.
- **The timer belongs to the mode that runs a script.** `refresh_interval` under `builtin` schedules nothing and is reported through `echoagent inspect`; under `disabled` it is off with everything else.

## Available data

Porting a script, read these closely. `workspace.repo_root` is the repository root, and there is no `project_dir`, a name used elsewhere for a launch directory. `context_window.session_usage` and the `session_*` token counts are cumulative for the session, not one call's, while the live window is `context_window.context_tokens`. There is no list of extra session directories, because EchoAgent has none. `transcript_path` names EchoAgent's own update stream rather than a transcript in another tool's format, and `prompt_id` is present only while a turn runs. In each case a ported script reads nothing rather than a wrong answer, so guard the ones you use.

Nothing outside the table below is sent. A ported script that reads counts of lines the agent changed, a rate-limit summary, an editor mode, a thinking or fast-mode flag, an output style, a pull request, extra session directories, or the directory a worktree was created from will find them absent: each is either a feature EchoAgent does not have or a number it cannot source honestly.

| Field | Description |
| --- | --- |
| `cwd`, `session_id` | Working directory and unique session id |
| `session_name` | The session's tab name, filled in by the client. Present in `command` stdin, absent from the `SessionStatus` notification |
| `prompt_id` | UUID of the prompt being processed. Present only during a turn |
| `transcript_path` | Path to the session's `updates.jsonl`. The file is EchoAgent's own update stream, so a script that parses another tool's transcript format will not read it |
| `model.id`, `model.display_name` | Model identifier and display name. Omitted when the agent cannot read the session's model |
| `workspace.current_dir` | Current directory |
| `workspace.repo_root` | The repository root, absent outside one. Not `project_dir`, a name used elsewhere for a launch directory |
| `workspace.branch` | Checked-out branch, in any repo. Absent on a detached HEAD |
| `workspace.git_worktree` | Worktree name, inside a linked worktree |
| `workspace.repo.{host,owner,name}` | Parsed from the `origin` remote, inside a git repo. `owner` is omitted for a remote with no owner segment |
| `schema_version` | Payload shape revision. Adding a field never bumps it; removing or retyping one does. Test it with `>=`, and branch on it rather than on `version` |
| `version` | EchoAgent release, for display |
| `cost.total_duration_ms` | Milliseconds since this process attached the session. A resumed session counts from the resume, as its cost does |
| `cost.total_cost_usd`, `cost.total_api_duration_ms` | Session cost and API-wait milliseconds. The cost is absent until something in the session carries a price, and also when the usage ledger is unreadable, so treat an absent cost as unknown rather than as zero |
| `context_window.context_window_size` | Maximum context size, in tokens. Omitted until the model's window is known |
| `context_window.context_tokens` | Tokens the conversation occupies right now, counting input only, so it falls after a compaction. Omitted when the agent cannot read the count, so `0` always means an empty context |
| `context_window.session_input_tokens`, `.session_output_tokens` | Billed across the whole session, so they only grow. Named for the session because that is what they count: `total_*` is used elsewhere for what is in the window right now, which here is `context_tokens`. Dividing these by `context_window_size` passes 100% and keeps going. Omitted when the usage ledger is unreadable |
| `context_window.used_percentage`, `.remaining_percentage` | How full the window is right now, whole numbers from 0 to 100. Omitted with `context_window_size` or `context_tokens`, since a percentage of an unknown window is not a number |
| `context_window.session_usage.{input_tokens,output_tokens,cache_creation_input_tokens,cache_read_input_tokens}` | `input_tokens`, `cache_creation_input_tokens` and `cache_read_input_tokens`, which sum back to `session_input_tokens`, plus `output_tokens`. Cumulative for the session, not one turn's. Absent before the first call |
| `context_window.auto_compact_threshold_percent` | Where the session auto-compacts. Omitted when the agent reported none |
| `effort.level` | Reasoning effort, when the model supports it |
| `turn.started_at_ms` | Unix milliseconds the turn in flight began, absent between turns. Subtract it from your own clock for an elapsed time |
| `worktree.{name,path,branch,main_worktree_root}` | Active worktree, inside a linked worktree. `name` is omitted for a worktree at a filesystem root, and `main_worktree_root` is where the worktree branched from |
| `trigger` | Why this run was invoked: `refresh_interval` for a run the timer asked for, `state` otherwise. Present on a command row's stdin, absent from the `SessionStatus` notification, which describes the session rather than a run |

Fields EchoAgent cannot source are omitted rather than sent as placeholders, so the row never shows a fabricated value. Always guard them: `jq -r` prints the literal text `null` for a missing key, so write `// 0` or `// "?"` in jq, and `?.` in JavaScript.

## Example

Save a script (for example `~/.echo-agent/statusline.sh`), make it executable with `chmod +x`, and set it as `command`. This one uses [`jq`](https://jqlang.org/); Python and Node.js parse JSON natively. The payload carries no dirty-file count, so the script calls `git` for that.

```bash
#!/bin/bash
input=$(cat)
DIR=$(echo "$input" | jq -r '.workspace.current_dir')
MODEL=$(echo "$input" | jq -r '.model.display_name // "?"')
PCT=$(echo "$input" | jq -r '.context_window.used_percentage // 0')
BRANCH=$(echo "$input" | jq -r '.workspace.branch // "detached"')
DIRTY=$(git diff --numstat 2>/dev/null | wc -l | tr -d ' ')
printf '%b\n' "${DIR##*/} │ $MODEL │ ${PCT}% ctx │ \033[32m$BRANCH\033[0m ~$DIRTY"
```

## Tips

- Test with mock input: `echo '{"session_id":"t","workspace":{"current_dir":"/tmp/demo"},"model":{"display_name":"EchoAgent 4.5"},"context_window":{"used_percentage":25}}' | ./statusline.sh`
- Cache slow commands such as `git status` to a temp file keyed on `session_id`, refreshed every few seconds. `session_id` is stable per session and unique across sessions.
- Use `printf '%b'` rather than `echo -e` for reliable escapes.

## Troubleshooting

- **Nothing shows.** EchoAgent reads `[ui.status_line]` at startup, so restart it after editing `config.toml`. Restarting is enough: when the new client attaches, the agent switches the row on for a session that is still running. The row only renders once the agent view is active, so not on the welcome screen or while a subagent view is open full screen. Check that `type` is not `disabled`, and that a command script is executable and writes to stdout.
- **A message in the row.** A row beginning `[ui.status_line]` means EchoAgent could not use that section as written: it either names the key it could not read, or names what the mode you chose still needs. `echoagent inspect` lists the same problems, including keys this version does not know, which is where to look when the row is switched off. Everything it could read still applies, and EchoAgent leaves the section as you wrote it rather than rewriting one it cannot read. Setting `type = "disabled"` removes the row and the message.
- **A blank row that never fills.** The agent is not sending status updates, which usually means a `echoagent` or leader process older than this client. Restart the leader or update EchoAgent.
- **Only your own config can set this.** A `command` row runs a program, so it is read from your `~/.echo-agent/config.toml` and from configuration your administrator manages. A repository cannot set one: a repo-local `.echo-agent/config.toml` is read for MCP servers only, and `[ui.status_line]` is not among the keys any project-scoped layer can supply, so cloning a repo cannot make EchoAgent run its script.
- **A pushed config had no effect.** `[ui.status_line]` is stripped from campaign and version-override patches, because a status line can name a command your machine would run. Set it in your own `config.toml`.
- **Errors.** Anything your script prints is shown, even when it exits non-zero, so `printf …; [[ -n $dirty ]]` behaves as you would expect. A script that prints nothing and fails shows `[status line: exit N]`, and that stays until the next run succeeds — for a run triggered by session state, which reports its failure at once; a timer run's failure keeps the last output instead (see [Refresh runs](#refresh-runs)). Your script's stderr is never painted, so a debugging `echo` will not disturb the row; run EchoAgent with `--debug` to read it. A script EchoAgent could not start at all shows `[status line: could not start the script: …]`, which is what a file without the execute bit produces, and one the system kills shows `[status line: killed by signal]`. A `#!` line naming a missing interpreter is retried under `sh` instead, so it shows an exit code.
