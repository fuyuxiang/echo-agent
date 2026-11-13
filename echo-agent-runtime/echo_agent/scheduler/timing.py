"""Validated single-occurrence and recurring schedule specifications."""
from datetime import datetime
from zoneinfo import ZoneInfo
import time

from echo_agent.scheduler.service import TriggerKind


def schedule_timing(params: dict) -> dict:
    zone = str(params.get("timezone") or "")
    tz = ZoneInfo(zone) if zone else None
    at = params.get("at")
    schedule = str(params.get("schedule") or "")
    if at and schedule:
        raise ValueError("Use either at (single occurrence) or schedule (recurring), not both")
    if at:
        moment = datetime.fromisoformat(str(at).replace("Z", "+00:00"))
        if moment.tzinfo is None:
            if tz is None:
                raise ValueError("at requires a UTC offset or an explicit IANA timezone")
            moment = moment.replace(tzinfo=tz)
        if moment.timestamp() <= time.time():
            raise ValueError("Reminder time must be in the future")
        return dict(trigger=TriggerKind.ONCE, at_ms=int(moment.timestamp() * 1000),
                    timezone=zone, cron_expr="", delete_after_run=True)
    if not schedule:
        raise ValueError("Provide at or schedule")
    from croniter import croniter
    cron = croniter(schedule, datetime.now(tz=tz))
    parts = schedule.split()
    # Five-field cron has no year. A fixed month/day silently became an
    # annual reminder before; recurring intent must now be explicit.
    if len(parts) == 5 and parts[2].isdigit() and parts[3].isdigit() and not params.get("recurring"):
        moment = cron.get_next(datetime)
        return dict(trigger=TriggerKind.ONCE, at_ms=int(moment.timestamp() * 1000),
                    timezone=zone, cron_expr="", delete_after_run=True)
    return dict(trigger=TriggerKind.CRON, cron_expr=schedule, timezone=zone,
                at_ms=0, delete_after_run=False)
