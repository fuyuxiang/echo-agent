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
