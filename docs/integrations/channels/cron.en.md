# Cron Channel

**Scope: standalone Python Runtime.** For Desktop operations, see the [Desktop guide](../../desktop/index.md).

The Cron channel triggers Agent execution on a schedule without external messages.

## One-time reminders and original attachments

Fixed reminders deliver saved text and original files directly, without inference at firing time. Use `cronjob` with an offset-aware ISO `at`, `delivery_text`, and conversation-scoped `attachment_ids`. Use `update` to amend the existing task. Agent workflows still use `command`; recurring schedules use `schedule`. A fixed month/day cron defaults to one occurrence unless yearly recurrence is explicit.

Files and instructions may arrive separately. With `await_attachment=true`, one unique file and one pending task in the same conversation are associated within a ten-minute window and confirmed. Ambiguous files are never selected automatically; explicit IDs remain available after the window. Originals live in `data/attachments/`, outside the evictable media cache, and must be included in workspace backups. Text extraction failure does not prevent forwarding originals. Use `attachment_list`, `read_document`, and `send_file` to find, read, or deliver them.

Text and each file have separate durable delivery intents and channel receipts. Recovery preserves confirmed parts and withholds uncertain sends. The Dashboard supports one-time times, direct reminder mode, attachment counts, and pending association status.

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

Originals have a workspace quota (`attachments.max_mb`, default 1024 MB) and expire after `attachments.retention_days` (default 30) when no reminder references them. Cleanup runs on subsequent saves or listing. Future/paused jobs and pending deliveries pin their originals. `attachment_delete` removes unreferenced originals in the current conversation; chat history and extracted text remain. Listings use a local index with automatic legacy import. A full store produces an explicit save failure, never silent eviction of a referenced file.

Automatic late binding applies only to a unique file without caption instructions. Pausing, deleting or editing a reminder cancels old pending delivery intents, including after restart; resume schedules future occurrences. Schedule completion and delivery receipts are shown separately. Routine edits never grant unattended privileges; a rename preserves existing consent and content changes require renewed authorization.
