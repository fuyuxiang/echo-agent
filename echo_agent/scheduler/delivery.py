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
    # 普通通道键为 channel:chat_id，或群聊 per_user 的 channel:chat_id:sender_id。
    # 投递目标始终是群/会话本身(chat_id)，需剥离末段 sender 后缀。
    # 前提(trusted-operator)：group-capable 通道的 chat_id 不含冒号。
    parts = session_key.split(":")
    channel = parts[0]
    chat_id = parts[1] if len(parts) >= 2 else ""
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

    # A scheduled job runs with no human at the keyboard, so mark it unattended
    # (otherwise the approval gate would route EXEC/DANGEROUS calls to a manual
    # prompt that never gets answered — the job would hang or fail at fire time).
    # The job exists in the scheduler store only because its creation went
    # through the DANGEROUS-tier cronjob approval, so it carries a per-job
    # authorization that lets the unattended resolver allow its work — scoped to
    # this job, without loosening the global unattended policy for anything else.
    # `unattended_authorized` defaults on but can be set False to force deny.
    payload_dict = job.payload if isinstance(job.payload, dict) else {}
    authorized = payload_dict.get("unattended_authorized", True)

    is_group = bool(payload.get("is_group", False))

    return InboundEvent(
        event_type=EventType.CRON,
        channel=target_channel,
        sender_id="cron",
        chat_id=target_chat_id,
        content=[ContentBlock(type=ContentType.TEXT, text=command)],
        session_key_override=session_key_override,
        # Trust signals go on the typed fields, not metadata: this is the only
        # producer allowed to grant them, and putting them on first-class fields
        # means no external channel payload can forge them (base._build_event
        # never sets these). The approval gate reads event.unattended /
        # event.cron_authorized directly.
        unattended=True,
        cron_authorized=bool(authorized),
        is_group=is_group,
        metadata={
            "job_id": job.id,
            "job_name": job.name,
            "source_session_key": source_session_key or session_key_override or "",
            "deliver_channel": target_channel,
            "deliver_chat_id": target_chat_id,
        },
    )


def build_scheduled_job_handler(bus: Any, *, inspection_runner: Any = None) -> Callable[[Any], Awaitable[str | None]]:
    async def _on_job(job: Any) -> str | None:
        payload = job.payload if isinstance(job.payload, dict) else {}
        if payload.get("_inspection_tick") and inspection_runner is not None:
            await inspection_runner()
            return "inspection"
        accepted = await bus.publish_inbound(inbound_event_from_job(job))
        if not accepted:
            raise RuntimeError(
                f"Scheduled job {job.id} was rejected by the bus (queue full or shutting down)"
            )
        # "queued", not "success": at this point the event is only accepted onto
        # the bus; the agent run and any user-facing delivery happen
        # asynchronously afterwards. The loop later calls
        # Scheduler.record_run_outcome to overwrite this with the terminal
        # "completed"/"error" once the turn finishes, so a job that never runs
        # or crashes downstream is no longer masked as a success.
        return "queued"

    return _on_job
