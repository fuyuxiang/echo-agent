from __future__ import annotations

from echo_agent.scheduler.delivery import inbound_event_from_job


class _Job:
    def __init__(self, payload):
        self.id = "j1"
        self.name = "n"
        self.payload = payload


def test_cron_event_carries_is_group_from_payload():
    job = _Job({"command": "ping", "channel": "slack", "chat_id": "C1", "is_group": True})
    ev = inbound_event_from_job(job)
    assert ev.is_group is True


def test_cron_event_defaults_not_group():
    job = _Job({"command": "ping", "channel": "slack", "chat_id": "C1"})
    ev = inbound_event_from_job(job)
    assert ev.is_group is False
