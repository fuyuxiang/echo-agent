# Cron Channel

The Cron channel triggers Agent execution on a schedule without external messages.

---

## Overview

Not a traditional "chat" channel — it's an event injection mechanism that sends predefined messages to the Agent on schedule, triggering automated workflows.

## Configuration

```yaml
channels:
  cron:
    enabled: true
```

## Capabilities

| Capability | Supported |
|-----------|-----------|
| Edit messages | ❌ |
| Reactions | ❌ |
| File send | ❌ |
| Realtime | ❌ |
| Group chat | ❌ |

## Creating Scheduled Jobs

### Via CLI

```bash
echo-agent cron list
echo-agent cron authorize <job-id>   # service must be stopped
echo-agent cron revoke <job-id>      # service must be stopped
```

### Via Agent Conversation

The Agent can create jobs using the `cronjob` tool:

> "Send me a weather report every morning at 9am"

It can also authorize or revoke an existing job:

> "Authorize scheduled job 148fb4a4b9"

### Via Dashboard

The Dashboard Cron page provides visual job management.

## Authorization

New cron jobs start unauthorized. The job still fires on schedule, but its
privileged work (writing files, running commands) is denied. This is security by
design:

- It stops the Agent from having scheduled jobs perform high-risk operations with nobody watching
- Authorization is issued per job, via three paths: say "authorize scheduled job `<id>`" in chat, tick the box on the Dashboard cron page, or stop the service and run `echo-agent cron authorize <id>`
- A grant is bound to the job's content: editing the instruction, schedule or delivery target invalidates it and re-authorization is required (renaming and pause/resume do not)

See the [scheduled jobs guide](../../guides/scheduled-jobs.en.md) for details.

## Output Routing

The cron channel has no send capability of its own. The delivery target is recorded **per job**, not globally — there is no `gateway.deliveryRoutes` routing table in the configuration.

Two fields on the job payload decide where output goes:

| Field | Description |
|-------|-------------|
| `deliver_channel` | Target channel name, e.g. `telegram` |
| `deliver_chat_id` | Target chat id |

When either is empty it falls back to the job's own `channel` / `chat_id`. Set them when creating the job through the `cronjob` tool or the Dashboard.
