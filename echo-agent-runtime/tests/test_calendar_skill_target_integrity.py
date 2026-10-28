"""Explicit CalDAV targets must not silently write to a different calendar."""

from __future__ import annotations

import importlib.util
import sys
from pathlib import Path
from types import SimpleNamespace

import pytest


@pytest.fixture
def calendar_skill(monkeypatch):
    source = (
        Path(__file__).resolve().parents[1]
        / "skills/productivity/calendar/scripts/calendar_client.py"
    )
    spec = importlib.util.spec_from_file_location("review_calendar_skill", source)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    monkeypatch.setattr(module, "require", lambda _feature: None)

    class Calendar:
        def __init__(self, name):
            self.name = name
            self.saved = []

        def save_event(self, content):
            self.saved.append(content)

    calendars = [Calendar("personal"), Calendar("work")]
    client = SimpleNamespace(
        principal=lambda: SimpleNamespace(calendars=lambda: calendars),
    )
    monkeypatch.setitem(
        sys.modules, "caldav", SimpleNamespace(DAVClient=lambda **_kwargs: client),
    )

    class ICalendar:
        def __init__(self):
            self.components = []

        def add_component(self, component):
            self.components.append(component)

        def to_ical(self):
            return b"BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n"

    class Event:
        def __init__(self):
            self.fields = {}

        def add(self, key, value):
            self.fields[key] = value

    monkeypatch.setitem(
        sys.modules, "icalendar", SimpleNamespace(Calendar=ICalendar, Event=Event),
    )
    return module, calendars


def _add(module, **kwargs):
    module.add_event(
        "https://calendar.invalid", "review-user", "review-password",
        "Harmless review event", "2026-10-21T09:00:00", **kwargs,
    )


def test_missing_explicit_calendar_never_saves_to_default(calendar_skill):
    module, calendars = calendar_skill
    with pytest.raises(ValueError, match="Calendar 'missing' not found"):
        _add(module, calendar_name="missing")
    assert all(not calendar.saved for calendar in calendars)


def test_matching_explicit_calendar_receives_the_event(calendar_skill):
    module, calendars = calendar_skill
    _add(module, calendar_name="work")
    assert calendars[0].saved == []
    assert len(calendars[1].saved) == 1


def test_omitted_calendar_keeps_first_calendar_default(calendar_skill):
    module, calendars = calendar_skill
    _add(module)
    assert len(calendars[0].saved) == 1
    assert calendars[1].saved == []
