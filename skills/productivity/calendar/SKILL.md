---
name: calendar
description: "Manage calendar events via CalDAV (Google/iCloud/Nextcloud) or local ICS files. View, create, and query events."
version: 1.0.0
metadata:
  echo:
    tags: [Calendar, Schedule, CalDAV, Events, Productivity]
---

# Calendar

Calendar management via CalDAV protocol or local ICS files.

## Configuration

`~/.echo-agent/calendar.yaml`:

```yaml
provider: caldav  # or "local"
caldav_url: https://caldav.icloud.com/
username: user@icloud.com
password_env: ECHO_CALDAV_PASS
local_ics: ~/.echo-agent/calendar.ics
```

## Script

```bash
pip install caldav icalendar

python3 scripts/calendar_client.py --url https://caldav.icloud.com/ --user me@icloud.com --password xxx list
python3 scripts/calendar_client.py --url https://caldav.icloud.com/ --user me@icloud.com --password xxx upcoming --days 7
python3 scripts/calendar_client.py --url https://caldav.icloud.com/ --user me@icloud.com --password xxx add "Team meeting" "2026-06-14T10:00" --end "2026-06-14T11:00"
```

## CalDAV Endpoints

| Provider | URL |
|----------|-----|
| iCloud | `https://caldav.icloud.com/` |
| Google | `https://apidata.googleusercontent.com/caldav/v2/` |
| Nextcloud | `https://your.server/remote.php/dav/` |

## Local ICS (Offline)

```python
from icalendar import Calendar, Event
from datetime import datetime

cal = Calendar()
event = Event()
event.add('summary', 'Meeting')
event.add('dtstart', datetime(2026, 6, 14, 10, 0))
event.add('dtend', datetime(2026, 6, 14, 11, 0))
cal.add_component(event)

with open('calendar.ics', 'wb') as f:
    f.write(cal.to_ical())
```

## Integration

- Feeds into `daily-briefing` skill for morning agenda
- Works with `reminder` skill for event-based reminders
