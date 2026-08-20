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
echo-agent cron authorize <job-id>
echo-agent cron revoke <job-id>
```

### Via Agent Conversation

The Agent can create jobs using the `cronjob` tool:

> "Send me a weather report every morning at 9am"

### Via Dashboard

The Dashboard Cron page provides visual job management.

## Authorization

New cron jobs require explicit authorization before execution (security by design):

```bash
echo-agent cron authorize <job-id>
```

## Output Routing

The cron channel has no send capability of its own. The delivery target is recorded **per job**, not globally — there is no `gateway.deliveryRoutes` routing table in the configuration.

Two fields on the job payload decide where output goes:

| Field | Description |
|-------|-------------|
| `deliver_channel` | Target channel name, e.g. `telegram` |
| `deliver_chat_id` | Target chat id |

When either is empty it falls back to the job's own `channel` / `chat_id`. Set them when creating the job through the `cronjob` tool or the Dashboard.
