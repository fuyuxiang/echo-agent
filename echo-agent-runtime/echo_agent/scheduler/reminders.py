"""Fixed reminders and late original-file binding, without an inference dependency."""
from __future__ import annotations

import time
from dataclasses import replace
from loguru import logger

from echo_agent.attachments import AttachmentStore
from echo_agent.bus.events import ContentBlock, ContentType, OutboundEvent
from echo_agent.scheduler.delivery import inbound_event_from_job
from echo_agent.scheduler.service import JobStatus


def reminder_delivery_allowed(job, run) -> bool:
    """A completed schedule can still await delivery; a cancelled intent cannot."""
    return bool(job and run and job.enabled and job.status not in {JobStatus.PAUSED, JobStatus.CANCELLED}
                and not run.get("delivery_cancelled")
                and run.get("reminder_payload", job.payload) == job.payload
                and run.get("scheduled_at_ms", job.at_ms) == job.at_ms)


def bind_pending_attachments(scheduler, store: AttachmentStore, session_key: str,
                             attachment_ids: list[str]) -> str:
    """Only one timely original and one explicit pending request can bind silently."""
    if scheduler is None or len(attachment_ids) != 1:
        return ""
    record = store.get(session_key, attachment_ids[0])
    if record["type"] != "file":
        return ""
    now = time.time()
    candidates = [job for job in scheduler.list_jobs() if (
        job.enabled and job.status == JobStatus.ACTIVE and not job.active_run_id
        and job.payload.get("source_session_key") == session_key
        and job.payload.get("delivery_mode") == "reminder"
        and job.payload.get("await_attachment") is True
        and not job.payload.get("attachment_ids")
        and job.payload.get("attachment_window_start", 0) <= record["received_at"]
        <= job.payload.get("attachment_window_end", 0)
        and now <= job.payload.get("attachment_window_end", 0)
    )]
    if len(candidates) != 1:
        return ""
    job = candidates[0]
    payload = dict(job.payload, attachment_ids=[record["id"]], await_attachment=False)
    # Fixed deliveries require no EXEC grant; never carry an old privileged
    # grant across the resolved attachment change.
    scheduler.update_job(job.id, payload=payload, set_authorization=True, authorization=None)
    return f"已收到并保存附件「{record['name']}」，已加入提醒「{job.name}」。届时会发送提醒内容和原文件，正文解析状态不影响发送。"


async def deliver_reminder(bus, job, *, recovery: bool = False) -> str:
    outputs = bus.scheduled_outputs
    if outputs is None:
        raise RuntimeError("Durable reminder delivery is unavailable")
    store = AttachmentStore(outputs.workspace)
    source = inbound_event_from_job(job)
    # One execution identity survives restarts and every delivery retry.
    source.event_id = f"reminder:{job.id}:{job.run_id}"
    outputs.register(source)
    payload = job.payload
    text = str(payload.get("delivery_text") or "").strip()
    if not text:
        raise ValueError("Reminder delivery_text is empty")
    blocks = [[ContentBlock(type=ContentType.TEXT, text=text)]]
    missing = []
    lookup = getattr(bus, "channel_lookup", None)
    adapter = lookup(source.channel) if callable(lookup) else None
    files_supported = not callable(lookup) or adapter is not None and getattr(adapter, "supports_files", False)
    for attachment_id in payload.get("attachment_ids", []):
        try:
            if not files_supported:
                raise ValueError("Channel cannot deliver files")
            record = store.get(source.session_key, attachment_id)
            blocks.append([ContentBlock(type=ContentType.FILE, url=str(store.path(record)),
                                        mime_type=record["mime_type"], metadata={"name": record["name"]})])
        except (ValueError, OSError):
            missing.append(attachment_id)
    if missing or payload.get("await_attachment"):
        blocks[0][0].text += ("\n\n当前渠道不支持原始附件投递，本次先发送提醒正文。" if not files_supported else
                             "\n\n附件尚未关联或原文件不可用，本次先发送提醒正文。")
    events = [OutboundEvent(channel=source.channel, chat_id=source.chat_id, content=content,
                delivery_origin=source, metadata={"scheduled_part": index, "scheduled_parts": len(blocks),
                    "reminder_delivery": True, "_inbound_event_id": source.event_id, "_tool_delivery": True})
              for index, content in enumerate(blocks)]
    # Persist ALL intents before the first network operation. Unsent remaining
    # files can recover after a crash even if the text already has its receipt.
    for event in events:
        current, run = outputs._run(event)
        if not reminder_delivery_allowed(current, run):
            await outputs.finished(source)
            return "incomplete"
        await outputs.prepare(event)
        existing = await outputs.store.get_outbound_event(event.event_id)
        await outputs.store.register_outbound(event)
        if existing is None:
            await outputs.store.update_outbound(event.event_id, "awaiting_context", "delivery pending",
                                               detail={"recovery_action": "retry_later"})
            await outputs.scheduler.record_delivery(job.id, job.run_id, {
                "event_id": event.event_id, "artifact_id": "", "status": "awaiting_context",
                "part": event.metadata["scheduled_part"], "total_parts": len(events),
                "error": "delivery pending", "channel": source.channel, "chat_id": source.chat_id,
            })
    successful = not missing and not payload.get("await_attachment")
    for event in events:
        current, run = outputs._run(event)
        if not reminder_delivery_allowed(current, run):
            await outputs.store.update_outbound(event.event_id, "superseded", "Reminder paused or edited")
            successful = False
            continue
        # On recovery, let channel checkpoints suppress confirmed parts and
        # withhold uncertain sends. No new event IDs or inferred receipts.
        if recovery:
            event.metadata["_delivery_retry"] = True
        result = await bus.publish_outbound(event)
        successful = successful and result.ok and not result.detail.get("deferred")
    await outputs.scheduler.record_run_outcome(job.id, "completed" if successful else "incomplete",
        "" if successful else "attachment_missing" if missing or payload.get("await_attachment") else "delivery_pending",
        run_id=job.run_id)
    await outputs.finished(source)
    return "completed" if successful else "incomplete"


async def resume_reminders(outputs) -> None:
    """Recover dispatch interrupted before all intents were persisted."""
    for job in outputs.scheduler.list_jobs():
        if job.payload.get("delivery_mode") != "reminder":
            continue
        for run in job.run_history:
            if not reminder_delivery_allowed(job, run):
                continue
            if run.get("status") not in {"dispatching", "queued", "interrupted", "error"} or not run.get("run_id"):
                continue
            # This path is safe ONLY for fixed deliveries with deterministic IDs.
            # General agent work keeps its existing inspect-before-retry rule.
            run.update(status="dispatching", error="")
            try:
                await deliver_reminder(outputs.bus, replace(job, run_id=run["run_id"],
                    payload=run.get("reminder_payload", job.payload)), recovery=True)
            except Exception as exc:
                # A corrupt or unavailable task must not prevent the gateway or
                # other reminders from recovering. Fixed intents keep their IDs.
                logger.exception("Could not resume reminder {}", job.id)
                await outputs.scheduler.record_run_outcome(job.id, "error", str(exc), run_id=run["run_id"])
