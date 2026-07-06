---
name: reminder
description: "Set reminders and manage todos with natural language. Uses built-in cron scheduling, no external service needed."
version: 1.0.0
metadata:
  echo:
    tags: [Reminder, Todo, Schedule, Cron, Productivity]
---

# Reminder

Set timed reminders and manage todos.

## IMPORTANT: how a reminder actually fires

A row in `reminders.db` is just a note — nothing scans it or delivers it. To make
a reminder that *actually fires and gets delivered*, you MUST create a scheduled
job with the **`cronjob` tool**, which persists to the scheduler and is delivered
through the active channel by Echo Agent's scheduler service:

```
cronjob(action="create", name="weekly-report",
        schedule="0 9 * * 1", command="提醒你写周报")
```

The `reminder_store.py` script below is only a lightweight local note-taking
list (list/done/delete). It has NO delivery path on its own. If the user wants
to be reminded at a time, use `cronjob` — optionally also recording a note via
the script. For a background one-off that must take effect, `spawn_task` can run
the `cronjob` call for you.

### Jobs that produce a file (audio/image/document)

When the scheduled command generates an artifact the user must receive (e.g. a
voice briefing), make the delivery happen inside the same tool call — do not
assume a later step will send it. For audio use `text_to_speech(..., deliver=true)`
(see the tts-voice skill); for other files call `send_file` explicitly with the
target channel/chat. An unattended cron run may end right after producing the
file, so "generate then hope it gets sent" silently drops the artifact.

### After creating a job: confirm, then stop

Once `cronjob(action="create")` returns, you are done. Send the user ONE
confirmation with the concrete facts and end the turn:

- job id, the schedule, and the next fire time (from the create result)
- if a delivery target was inferred, say so; if the create result carried the
  "⚠️ 无法确定投递目标" warning, relay it and ask for target_channel/target_chat_id.

Do NOT try to "prove delivery works" by exercising the agent's own plumbing —
reading `gateway/server.py`, `a2a/server.py`, or curling internal endpoints like
`/v1/chat` or `/api/v1/message` is not verification, it's a rabbit hole that
burns the turn and leaves the user with no reply. If you genuinely need to sanity
-check the schedule, use `cronjob(action="list")`; trigger a real run at most
once and only if the user asked. The scheduled job itself is the delivery test —
it will fire on schedule.

## Quick Commands

| Action | Example |
|--------|---------|
| Set reminder | "提醒我明天9点开会" |
| Recurring | "每周一早上8点提醒我写周报" |
| List | "显示我的所有提醒" |
| Complete | "完成提醒 #3" |
| Delete | "删除提醒 #5" |

## Time Parsing

Natural language to cron expression mapping. Feed the resulting expression into
the `cronjob` tool's `schedule` argument:

| Input | Cron Expression |
|-------|----------------|
| 明天9点 | `0 9 {tomorrow} * *` |
| 每天早上8点 | `0 8 * * *` |
| 每周一 | `0 9 * * 1` |
| 每月1号 | `0 9 1 * *` |
| 2小时后 | one-shot timer |
| 工作日下午5点 | `0 17 * * 1-5` |

## Local note storage (no delivery)

The script keeps a local note list in SQLite: `~/.echo-agent/reminders.db`.
This is bookkeeping only — creating a row here does NOT schedule or deliver
anything. Timed delivery is the `cronjob` tool's job (see top of this doc).

```sql
CREATE TABLE reminders (
    id INTEGER PRIMARY KEY,
    content TEXT NOT NULL,
    cron_expr TEXT,
    next_fire DATETIME,
    is_recurring BOOLEAN DEFAULT 0,
    is_done BOOLEAN DEFAULT 0,
    created_at DATETIME DEFAULT CURRENT_TIMESTAMP
);
```

## Script Usage (local notes only — does not schedule delivery)

```bash
python3 scripts/reminder_store.py add "写周报" --cron "0 9 * * 1"
python3 scripts/reminder_store.py list
python3 scripts/reminder_store.py done 3
python3 scripts/reminder_store.py delete 5
python3 scripts/reminder_store.py due  # show due reminders
```

## Delivery

Delivery is handled by the scheduler, not by this script. When you create a job
with the `cronjob` tool, the scheduler fires it on schedule and routes the
message through the active channel (Telegram/WeChat/etc). The `reminder_store.py`
script never delivers anything by itself.
