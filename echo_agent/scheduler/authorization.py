"""Per-job unattended-execution authorization.

A cron job may only run WRITE/EXEC tools with no human present if a human
authorized *this* job's content. Authorization used to be inferred from a
missing payload key defaulting to True, which meant any caller able to create a
job also granted itself that permission. It is now an explicit first-class
field on ScheduledJob, bound to the job's content by a fingerprint.

Binding it to a fingerprint is what makes "editing a job revokes its
authorization" a property rather than a rule each write path must remember to
enforce: the instruction, the delivery target and the firing schedule all feed
the hash, so changing any of them invalidates the grant automatically.

`name` and `enabled` are deliberately excluded. Renaming a job, or pausing and
resuming one whose content never changed, does not change what a human
consented to. The dangerous path — pause, edit the instruction, resume — is
already covered by the instruction hash.
"""
from __future__ import annotations

import hashlib
import json
import time
from dataclasses import dataclass
from typing import Any

# Bump when the fingerprint inputs or hashing change. verify() refuses any
# other version outright instead of keeping legacy algorithms alive: an old
# grant computed under different rules is not evidence about today's content.
AUTHORIZATION_SCHEMA_VERSION = 1

_SUMMARY_MAX = 120


@dataclass(frozen=True)
class JobAuthorization:
    operator: str = ""
    source: str = ""
    granted_at_ms: int = 0
    fingerprint: str = ""
    schema_version: int = AUTHORIZATION_SCHEMA_VERSION
    # Human-readable audit breadcrumb only. Deliberately NOT part of the
    # fingerprint — it is derived from the instruction, so hashing it would
    # double-count and make the summary's own truncation rules load-bearing.
    summary: str = ""

    def to_dict(self) -> dict[str, Any]:
        return {
            "operator": self.operator,
            "source": self.source,
            "granted_at_ms": self.granted_at_ms,
            "fingerprint": self.fingerprint,
            "schema_version": self.schema_version,
            "summary": self.summary,
        }

    @classmethod
    def from_dict(cls, data: Any) -> JobAuthorization | None:
        if not isinstance(data, dict):
            return None
        try:
            return cls(
                operator=str(data.get("operator", "")),
                source=str(data.get("source", "")),
                granted_at_ms=int(data.get("granted_at_ms", 0) or 0),
                fingerprint=str(data.get("fingerprint", "")),
                schema_version=int(data.get("schema_version", 0) or 0),
                summary=str(data.get("summary", "")),
            )
        except (TypeError, ValueError):
            # Corrupt record reads as absent, which verify() treats as
            # unauthorized — the safe direction.
            return None


def _instruction(job: Any) -> str:
    """The instruction delivery will actually run.

    `command` and `message` are two spellings of one logical slot and
    delivery.inbound_event_from_job prefers `command`; mirror that exactly, or a
    job could be authorized against text that never executes.
    """
    payload = job.payload if isinstance(job.payload, dict) else {}
    return str(payload.get("command") or payload.get("message") or "").strip()


def compute_fingerprint(job: Any) -> str:
    payload = job.payload if isinstance(job.payload, dict) else {}
    material = {
        "instruction": _instruction(job),
        "delivery": [
            str(payload.get("deliver_channel") or payload.get("channel") or "").strip(),
            str(payload.get("deliver_chat_id") or payload.get("chat_id") or "").strip(),
            str(payload.get("source_session_key") or payload.get("session_key") or "").strip(),
        ],
        "trigger": [
            str(getattr(job, "cron_expr", "") or ""),
            int(getattr(job, "interval_ms", 0) or 0),
            int(getattr(job, "at_ms", 0) or 0),
            str(getattr(job, "timezone", "") or ""),
        ],
    }
    blob = json.dumps(material, ensure_ascii=False, sort_keys=True, separators=(",", ":"))
    return hashlib.sha256(blob.encode("utf-8")).hexdigest()


def grant(job: Any, *, operator: str, source: str) -> JobAuthorization:
    """Issue an authorization for the job's CURRENT content."""
    instruction = _instruction(job)
    return JobAuthorization(
        operator=operator or "unknown",
        source=source,
        granted_at_ms=int(time.time() * 1000),
        fingerprint=compute_fingerprint(job),
        schema_version=AUTHORIZATION_SCHEMA_VERSION,
        summary=instruction.splitlines()[0][:_SUMMARY_MAX] if instruction else "",
    )


def verify(job: Any) -> bool:
    """True only if this job carries a valid grant for its current content."""
    auth = getattr(job, "authorization", None)
    if not isinstance(auth, JobAuthorization):
        return False
    if auth.schema_version != AUTHORIZATION_SCHEMA_VERSION:
        return False
    if not auth.fingerprint:
        return False
    try:
        expected = compute_fingerprint(job)
    except (TypeError, ValueError):
        # A hand-edited store can hold a non-numeric interval_ms/at_ms, which
        # int() rejects. "Cannot be fingerprinted" is not evidence of consent, so
        # it reads as unauthorized — the same direction from_dict takes, and it
        # keeps callers on serialization paths from turning this into a 500.
        return False
    return auth.fingerprint == expected
