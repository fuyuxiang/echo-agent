---
name: reminder
description: "Set reminders and manage todos with natural language. Uses built-in cron scheduling, no external service needed."
version: 1.0.0
metadata:
  echo:
    tags: [Reminder, Todo, Schedule, Cron, Productivity]
---

# Reminder

Set timed reminders and manage todos. Uses Echo Agent's built-in cron channel and SQLite storage.

## Quick Commands

| Action | Example |
|--------|---------|
| Set reminder | "提醒我明天9点开会" |
| Recurring | "每周一早上8点提醒我写周报" |
| List | "显示我的所有提醒" |
| Complete | "完成提醒 #3" |
| Delete | "删除提醒 #5" |

## Time Parsing

Natural language to cron expression mapping:

| Input | Cron Expression |
|-------|----------------|
| 明天9点 | `0 9 {tomorrow} * *` |
| 每天早上8点 | `0 8 * * *` |
| 每周一 | `0 9 * * 1` |
| 每月1号 | `0 9 1 * *` |
| 2小时后 | one-shot timer |
| 工作日下午5点 | `0 17 * * 1-5` |

## Storage

Reminders stored in SQLite: `~/.echo-agent/reminders.db`

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

## Script Usage

```bash
python3 scripts/reminder_store.py add "写周报" --cron "0 9 * * 1"
python3 scripts/reminder_store.py list
python3 scripts/reminder_store.py done 3
python3 scripts/reminder_store.py delete 5
python3 scripts/reminder_store.py due  # show due reminders
```

## Integration with Cron Channel

Echo Agent's cron channel checks for due reminders and delivers through active channels (Telegram/WeChat/etc).
