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

Cron channel has no send capability. Output is routed to other channels via Gateway Delivery Router:

```yaml
gateway:
  deliveryRoutes:
    - source: cron
      target: telegram
```
