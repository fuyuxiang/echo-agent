"""Delivery receipt value objects: track an outbound event's real fate.

A published outbound event is not the same as a delivered one. This module
lets publish_outbound report which stage delivery actually reached so that
cron jobs / background tasks / HTTP waits stop reporting success for messages
that never left the process.
"""
from __future__ import annotations

from dataclasses import dataclass, field
from enum import Enum
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from echo_agent.channels.base import SendResult


class DeliveryStage(str, Enum):
    GENERATED = "generated"      # content produced, not yet on the bus
    ENQUEUED = "enqueued"        # accepted onto the bus/queue
    ACCEPTED = "accepted"        # a handler took it, but gave no platform receipt
    DELIVERED = "delivered"      # platform confirmed delivery
    FAILED = "failed"            # explicit failure
    NO_HANDLER = "no_handler"    # no subscriber; silently dropped


@dataclass
class DeliveryResult:
    stage: DeliveryStage
    channel: str
    error: str | None = None
    detail: dict = field(default_factory=dict)

    @property
    def ok(self) -> bool:
        # Decision: ACCEPTED counts as success (conservative compatibility) —
        # handlers returning None have no receipt capability, so we cannot fault
        # them; only an explicit failure or exception is treated as failed.
        return self.stage in (DeliveryStage.DELIVERED, DeliveryStage.ACCEPTED)

    @classmethod
    def from_send_result(cls, sr: "SendResult", channel: str) -> "DeliveryResult":
        # A channel can intentionally skip a non-final event (for example an
        # uneditable channel suppressing a progress chunk). That is successful
        # handling, but it is not platform delivery. Preserve the historical
        # ``ok`` compatibility of ACCEPTED while letting callers that require a
        # real receipt (notably approval prompts) distinguish the two.
        if sr.skipped:
            return cls(DeliveryStage.ACCEPTED, channel, detail={"skipped": True})
        if sr.success:
            return cls(DeliveryStage.DELIVERED, channel, detail={"message_id": sr.message_id})
        return cls(DeliveryStage.FAILED, channel, error=sr.error or "send failed")
