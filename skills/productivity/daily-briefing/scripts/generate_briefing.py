#!/usr/bin/env python3
"""Daily briefing: aggregates weather, reminders, calendar, news."""

import argparse
import json
import sys
import urllib.request
from datetime import date, datetime


def get_weather(city="Beijing"):
    try:
        url = f"https://wttr.in/{city}?format=j1"
        data = json.loads(urllib.request.urlopen(url, timeout=10).read())
        cur = data["current_condition"][0]
        return f"Weather: {cur['weatherDesc'][0]['value']}, {cur['temp_C']}°C, humidity {cur['humidity']}%"
    except Exception as e:
        return f"Weather: unavailable ({e})"


def get_reminders():
    try:
        from pathlib import Path
        sys.path.insert(0, str(Path(__file__).resolve().parent.parent.parent / "reminder" / "scripts"))
        from reminder_store import get_due
        return get_due()
    except Exception:
        return []


def generate_briefing(city="Beijing"):
    lines = [f"# Daily Briefing — {date.today()}", ""]
    lines.append(get_weather(city))
    lines.append("")
    lines.append(f"Date: {datetime.now().strftime('%Y-%m-%d %A')}")
    lines.append("")

    reminders = get_reminders()
    if reminders:
        lines.append("## Due Reminders")
        for r in reminders:
            lines.append(f"  - #{r[0]}: {r[1]}")
    else:
        lines.append("No pending reminders.")

    return "\n".join(lines)


def main():
    parser = argparse.ArgumentParser(description="Generate daily briefing")
    parser.add_argument("--city", default="Beijing", help="City for weather")
    args = parser.parse_args()
    print(generate_briefing(args.city))


if __name__ == "__main__":
    main()
