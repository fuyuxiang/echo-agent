"""Scheduled job delivery helpers."""

from __future__ import annotations

from typing import Any, Awaitable, Callable

from echo_agent.bus.events import ContentBlock, ContentType, EventType, InboundEvent


def target_from_session_key(session_key: str) -> tuple[str, str]:
    if not session_key or ":" not in session_key:
        return "", ""
    if session_key.startswith("gateway:"):
        parts = session_key.split(":", 2)
        if len(parts) == 3 and parts[1] and parts[2]:
            return f"gateway:{parts[1]}", parts[2]
        return "", ""
    channel, chat_id = session_key.split(":", 1)
    return (channel, chat_id) if channel and chat_id else ("", "")


def inbound_event_from_job(job: Any) -> InboundEvent:
    payload = job.payload if isinstance(job.payload, dict) else {}
    command = str(payload.get("command") or payload.get("message") or "").strip()
    if not command:
        raise ValueError(f"Scheduled job {job.id} has no command payload")

    source_session_key = str(payload.get("source_session_key") or payload.get("session_key") or "").strip()
    target_channel = str(payload.get("deliver_channel") or payload.get("channel") or "").strip()
    target_chat_id = str(payload.get("deliver_chat_id") or payload.get("chat_id") or "").strip()
    if (not target_channel or not target_chat_id) and source_session_key:
        session_channel, session_chat_id = target_from_session_key(source_session_key)
        target_channel = target_channel or session_channel
        target_chat_id = target_chat_id or session_chat_id

    session_key_override = source_session_key or None
    if not target_channel or not target_chat_id:
        target_channel = "cron"
        target_chat_id = f"cron:{job.id}"
        session_key_override = f"cron:{job.id}"

    return InboundEvent(
        event_type=EventType.CRON,
        channel=target_channel,
        sender_id="cron",
        chat_id=target_chat_id,
        content=[ContentBlock(type=ContentType.TEXT, text=command)],
        session_key_override=session_key_override,
        metadata={
            "job_id": job.id,
            "job_name": job.name,
            "source_session_key": source_session_key or session_key_override or "",
            "deliver_channel": target_channel,
            "deliver_chat_id": target_chat_id,
        },
    )


def build_scheduled_job_handler(bus: Any) -> Callable[[Any], Awaitable[None]]:
    async def _on_job(job: Any) -> None:
        await bus.publish_inbound(inbound_event_from_job(job))

    return _on_job
