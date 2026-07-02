# echo_agent/cli/tui/protocol.py
"""Pure helpers for parsing cognitive frames and translating approval
decisions. No I/O — shared by every TUI widget and easily unit-tested."""

from __future__ import annotations

from dataclasses import dataclass

COG_TYPES = frozenset({
    "memory_recalled", "memory_written", "thinking", "tool_call",
    "approval_request", "cost_update", "heartbeat", "evolution",
})


@dataclass
class CogEvent:
    cog_type: str
    cog_event_id: str
    inbound_event_id: str
    data: dict
    summary: str


def parse_cog_frame(payload: dict) -> CogEvent | None:
    if payload.get("message_kind") != "cognitive":
        return None
    meta = payload.get("metadata") or {}
    cog_type = meta.get("cog_type", "")
    if cog_type not in COG_TYPES:
        return None
    return CogEvent(
        cog_type=cog_type,
        cog_event_id=str(meta.get("cog_event_id", "")),
        inbound_event_id=str(meta.get("_inbound_event_id", "")),
        data=meta.get("data") or {},
        summary=payload.get("text") or "",
    )


def approve_command(request_id: str, level: str = "") -> str:
    return f"/approve {request_id} {level}".strip()


def deny_command(request_id: str, reason: str = "") -> str:
    return f"/deny {request_id} {reason}".strip()


class CogDedup:
    """Tracks seen cog_event_ids so reconnect re-delivery doesn't double-render."""

    def __init__(self, max_entries: int = 512) -> None:
        self._seen: dict[str, None] = {}
        self._max = max_entries

    def seen(self, cog_event_id: str) -> bool:
        if cog_event_id in self._seen:
            return True
        self._seen[cog_event_id] = None
        while len(self._seen) > self._max:
            self._seen.pop(next(iter(self._seen)))
        return False
