---
name: daily-briefing
description: "Daily briefing aggregating weather, reminders, news, and calendar. Delivered on schedule via any channel."
version: 1.0.0
metadata:
  echo:
    tags: [Briefing, Daily, Morning, Digest, Productivity]
---

# Daily Briefing

Generates a morning briefing combining weather, today's tasks, news headlines, and calendar events.

## Configuration

Create `~/.echo-agent/briefing.yaml`:

```yaml
schedule: "0 8 * * *"  # 每天早上8点
location: Beijing
sections:
  - weather
  - reminders
  - rss
  - calendar
rss_feeds:
  - https://hnrss.org/frontpage
  - https://arxiv.org/rss/cs.AI
max_news: 5
channel: telegram  # delivery channel
```

## Briefing Template

```markdown
☀ 每日简报 — {date}

## 天气
{location}: {condition} {temp}°C, 湿度 {humidity}%

## 今日待办
- [ ] {reminder_1}
- [ ] {reminder_2}

## 新闻摘要
1. {title_1} — {source}
2. {title_2} — {source}

## 日程
- 09:00 {event_1}
- 14:00 {event_2}
```

## Script Usage

```bash
# Generate briefing now
python3 scripts/generate_briefing.py

# Generate with custom config
python3 scripts/generate_briefing.py --config ~/.echo-agent/briefing.yaml

# Preview without sending
python3 scripts/generate_briefing.py --dry-run
```

## Scheduling

Add to Echo Agent's cron channel for automatic daily delivery. The cron expression `0 8 * * *` fires at 8:00 AM daily.

Create the job with the `cronjob` tool. If the briefing produces a file (e.g. a
voice version), deliver it inside the same run — for audio use
`text_to_speech(..., deliver=true)`, for other files call `send_file` with the
target channel/chat. An unattended run may end right after generating the file,
so a "generate now, send later" split silently drops it.

### After creating the job: confirm, then stop

When `cronjob(action="create")` returns, send the user ONE confirmation — job id,
schedule, next fire time — and end the turn. Do NOT "verify delivery" by reading
`gateway/server.py` / `a2a/server.py` or curling internal endpoints
(`/v1/chat`, `/api/v1/message`); that is not a delivery test, it just burns the
turn and leaves the user without a reply. To sanity-check, use
`cronjob(action="list")`; the scheduled fire is the real delivery.
