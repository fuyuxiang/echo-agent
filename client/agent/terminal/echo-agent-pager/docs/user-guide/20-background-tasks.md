# Background Tasks and Monitoring

EchoAgent runs long-lived processes without blocking the conversation. This document covers background commands, the `/loop` command, the `monitor` tool, and the scheduler.

---

## Background Commands

Set `background: true` on the `run_terminal_command` tool to run a command in the background. It returns a task ID immediately; retrieve output with `get_command_or_subagent_output`.

### How It Works

1. The agent calls `run_terminal_command` with `background: true`.
2. The command starts in the background.
3. The agent receives a `task_id` for later reference.
4. When the command completes, a notification appears in the conversation.

### Getting Output

Use the `get_command_or_subagent_output` tool to check on a background command or subagent:

- `get_command_or_subagent_output(task_id)` — current output and status without waiting
- `get_command_or_subagent_output(task_id, timeout_ms=30000)` — wait up to the given milliseconds for completion

### Waiting for Multiple Tasks

Use `wait_commands_or_subagents` to block on several tasks at once:

- `task_ids` — the list of task IDs to wait for (maximum 20)
- `mode` — `wait_any` returns when the first task completes; `wait_all` waits for every task
- `timeout_ms` — the maximum time to wait, in milliseconds (default: 30 seconds)

The tool returns the status and output for every task you list.

### Killing Background Tasks

Use `kill_command_or_subagent(task_id)` to terminate a running background task or subagent. The tool sends SIGTERM, then SIGKILL, to shell processes, and sends Cancel and Shutdown to subagents. It reports success if the task was killed or had already exited.

### Common Use Cases

- **Dev servers**: Start a development server and continue coding
- **Test suites**: Run tests in the background while working on fixes
- **Build processes**: Start a build and check results later
- **Long compilations**: Start a compile and continue with other tasks

---

## Send a Running Task to the Background

In the interactive TUI, press `Ctrl+B` to send the running foreground command to the background. It is the only backgrounding shortcut, though sending a new message mid-command also backgrounds that command instead of killing it. Do this when:

- A command takes longer than expected.
- You want to ask the agent something else while a command runs.
- You realize a process is long-running after it has started.

The task keeps running, and you receive a notification when it completes.

---

## The /loop Command

`/loop` runs a prompt on a recurring interval. It is useful for polling tasks, periodic checks, and continuous monitoring.

### Syntax

```
/loop [interval] <prompt>
```

The interval format supports:

| Format | Example | Description        |
| ------ | ------- | ------------------ |
| `Ns`   | `60s`   | Every N seconds (minimum 60) |
| `Nm`   | `5m`    | Every N minutes    |
| `Nh`   | `2h`    | Every N hours      |
| `Nd`   | `1d`    | Every N days       |

### Examples

```
/loop 5m Check if the test suite passes and report any failures
/loop 2h Summarize new commits since the last check
/loop 60s Check if the dev server at localhost:3000 is responding
```

### Behavior

- The prompt fires immediately on creation, then repeats at the specified interval
- Each firing creates a new agent turn
- Recurring tasks auto-expire after 7 days
- Maximum 50 scheduled tasks can be active at once

---

## The monitor Tool

The `monitor` tool streams events from a long-running script. Each line of output becomes a notification in the conversation. The `monitor` tool is the streaming counterpart to `/loop`: use `/loop` for periodic checks, and use `monitor` for real-time event streams.

### How It Works

1. You provide a shell command (`command`) and a short `description` that appears in every notification.
2. EchoAgent merges the command's stdout and stderr into a single output file.
3. Each new line in that file becomes a notification delivered to the conversation.
4. The monitor runs until the command exits or you stop it.

### Script Guidelines

- **Always use `grep --line-buffered` in pipes.** Without it, pipe buffering delays events by minutes.
- **Handle transient failures in poll loops** (`curl ... || true`). One failed request should not stop the monitor.
- **Use selective filters.** Every line becomes a message, so never pipe raw logs.
- **Set poll intervals to match the source.** Use 30 seconds or more for remote APIs to respect rate limits, and 0.5 to 1 second for local checks.
- **Both stdout and stderr generate events.** Redirect output you don't want as events — for example, append `2>/dev/null` — or filter it out.

### Examples

```bash
# Watch for errors in a log file
tail -f /var/log/app.log | grep --line-buffered "ERROR"

# Monitor file changes in a directory
inotifywait -m --format '%e %f' /watched/dir

# Poll GitHub for new PR comments
last=$(date -u +%Y-%m-%dT%H:%M:%SZ)
while true; do
  now=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  gh api "repos/owner/repo/issues/123/comments?since=$last" \
    --jq '.[] | "\(.user.login): \(.body)"'
  last=$now; sleep 30
done
```

### Persistent Monitors

Set `persistent: true` for monitors that should run for the lifetime of the session:

- PR monitoring
- Log tailing
- CI status watching

Stop persistent monitors with `kill_command_or_subagent(task_id)`.

### Volume Control

If a monitor produces too many events, EchoAgent stops it automatically. When this happens, restart the monitor with a tighter filter. Prefer `grep --line-buffered`, `awk`, or a wrapper script that emits only the events you care about.

---

## The Scheduler

The scheduler provides a lower-level API for creating recurring tasks. `/loop` is a convenience wrapper around the scheduler.

### scheduler_create

Create a scheduled task:

| Parameter        | Description                                              |
| ---------------- | -------------------------------------------------------- |
| `interval`       | How often to run: `"5m"`, `"2h"`, `"1d"`, `"60s"`       |
| `prompt`         | The prompt text to execute on each fire                  |
| `fire_immediately`| Fire on creation in addition to the interval (default: `false`) |
| `recurring`      | Repeat (default: `true`) or fire once (`false`)          |
| `durable`        | Persist across sessions (default: `false`)               |

### scheduler_list

List all active scheduled tasks with their IDs, prompts, intervals, and next fire times.

### scheduler_delete

Cancel a scheduled task by ID. Returns success if the task was found and removed.

---

## The Tasks Pane

In the interactive TUI, press `Ctrl+G` to toggle the tasks pane. This pane lists, in a single view:

- Running subagents and their progress
- Active background tasks and their status
- Monitor and `/loop` tasks, each with a live line-count badge
- The task ID for each entry

To toggle the prompt queue instead, press `Ctrl+;`.

---

## The Still-Running Status Line

Whenever background work is still running while the agent looks idle — between turns, or while a turn is blocked on a user-interruptible wait — a persistent status line appears above the prompt:

```
◎ 1 command · 2 monitors · 1 loop · 1 subagent still running
```

It counts running background commands, monitors, scheduled `/loop` tasks, and background subagents, and updates live as each finishes. Any of them can wake the agent for a new turn (commands and subagents on completion, monitors on events, loops on their timer), so the cue stays up until nothing is left. The running counts live only on this status line: completions land in the transcript as a single "Task completed" chip, and "Worked for" markers stay plain — the transcript never repeats or restates the running counts.

While a turn is waiting on background work (blocked in a `get_task_output` or `wait_tasks` call), the status line adds a hint that typing takes over immediately:

```
◎ 1 command still running · send a message to interrupt
```

The same hint appears as `◎ waiting · send a message to interrupt` when the agent is waiting on something with no live counter (a sleep, or work that already finished). Sending a message interrupts the wait and runs your message right away. The transcript keeps its usual shape throughout: one "Worked for" marker when the turn ends. When a completion wakes the agent and it replies, that reply gets its own "Worked for" marker; a wake the agent answers silently leaves no trace in the transcript — unless it fails, in which case a "Turn failed" line appears even for a silent wake, so a standing instruction never stops executing invisibly.

---

## Use Cases and Patterns

### Dev Server + Coding

Start a dev server in the background and continue coding:

```
Start the dev server with `npm run dev` in the background, then implement the login form.
```

The agent runs the dev server with `background: true` and continues writing code. When the server starts, you see a notification.

### Continuous Test Monitoring

```
/loop 5m Run the test suite and report any new failures since the last run
```

Every 5 minutes, the agent runs tests and reports only new failures.

### Log Monitoring

Use `monitor` to watch for specific events:

```
Monitor the application log for ERROR and WARN entries. Use:
tail -f /var/log/app.log | grep --line-buffered -E "ERROR|WARN"
```

Each error or warning appears as a notification in the conversation.

### CI Pipeline Watching

```
/loop 2m Check the status of the GitHub Actions run for this PR. Report when it completes.
```

---

## Best Practices

- **Use `background` for one-shot long commands** (builds, test suites, server starts)
- **Use `/loop` for periodic checks** (CI status, test runs, health checks)
- **Use `monitor` for real-time event streams** (log tailing, file watching)
- **Use `scheduler_create` with `recurring: false`** for delayed one-shot tasks
- **Keep monitor filters tight** — prefer `grep --line-buffered` over raw log streams
- **Do not use sleep loops** in normal commands to poll — use `get_command_or_subagent_output` with `timeout_ms` instead
- **Set reasonable poll intervals** — 30s+ for remote APIs to avoid rate limits, shorter for local checks
