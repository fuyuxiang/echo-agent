#!/usr/bin/env python3
"""CalDAV calendar client: list, add, upcoming events."""

import argparse
from echo_agent.dependencies.skill_require import require  # noqa: E402
from datetime import datetime, timedelta

import caldav  # noqa: E402
from icalendar import Calendar, Event  # noqa: E402


def _get_client(url, username, password):
    require("skill.calendar")
    return caldav.DAVClient(url=url, username=username, password=password)


def list_calendars(url, username, password):
    client = _get_client(url, username, password)
    principal = client.principal()
    for cal in principal.calendars():
        print(f"  {cal.name} ({cal.url})")


def upcoming(url, username, password, days=7, calendar_name=None):
    client = _get_client(url, username, password)
    principal = client.principal()
    calendars = principal.calendars()
    if calendar_name:
        calendars = [c for c in calendars if c.name == calendar_name]

    start = datetime.now()
    end = start + timedelta(days=days)

    for cal in calendars:
        events = cal.date_search(start=start, end=end, expand=True)
        for event in events:
            vevent = event.vobject_instance.vevent
            summary = str(vevent.summary.value) if hasattr(vevent, "summary") else "No title"
            dtstart = vevent.dtstart.value
            print(f"  [{dtstart}] {summary}")


def add_event(url, username, password, summary, start_str, end_str=None, calendar_name=None):
    require("skill.calendar")
    client = _get_client(url, username, password)
    principal = client.principal()
    calendars = principal.calendars()
    cal = calendars[0]
    if calendar_name:
        cal = next((c for c in calendars if c.name == calendar_name), cal)

    start = datetime.fromisoformat(start_str)
    end = datetime.fromisoformat(end_str) if end_str else start + timedelta(hours=1)

    ical = Calendar()
    ev = Event()
    ev.add("summary", summary)
    ev.add("dtstart", start)
    ev.add("dtend", end)
    ical.add_component(ev)
    cal.save_event(ical.to_ical().decode())
    print(f"Event added: {summary} @ {start}")


def main():
    parser = argparse.ArgumentParser(description="CalDAV calendar client")
    parser.add_argument("--url", required=True, help="CalDAV server URL")
    parser.add_argument("--user", required=True)
    parser.add_argument("--password", required=True)
    sub = parser.add_subparsers(dest="cmd")
    sub.add_parser("list")
    p = sub.add_parser("upcoming")
    p.add_argument("--days", type=int, default=7)
    p.add_argument("--calendar")
    p = sub.add_parser("add")
    p.add_argument("summary")
    p.add_argument("start")
    p.add_argument("--end")
    p.add_argument("--calendar")
    args = parser.parse_args()

    if args.cmd == "list":
        list_calendars(args.url, args.user, args.password)
    elif args.cmd == "upcoming":
        upcoming(args.url, args.user, args.password, args.days, args.calendar)
    elif args.cmd == "add":
        add_event(args.url, args.user, args.password, args.summary, args.start, args.end, args.calendar)
    else:
        parser.print_help()


if __name__ == "__main__":
    main()
