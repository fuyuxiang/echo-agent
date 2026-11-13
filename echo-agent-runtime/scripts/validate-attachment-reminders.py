#!/usr/bin/env python3
"""Installed-package smoke check. All channel transports are mocked; sends nothing."""
from __future__ import annotations

import argparse
import asyncio
import hashlib
import json
import shutil
import tempfile
from datetime import datetime, timedelta, timezone
from pathlib import Path
from unittest.mock import AsyncMock, MagicMock, patch

from echo_agent.agent.context import ContextBuilder
from echo_agent.agent.media.document_extract import extract
from echo_agent.agent.tools.cronjob import CronjobTool
from echo_agent.agent.tools.filesystem import ReadFileTool
from echo_agent.scheduler.authorization import grant, verify
from echo_agent.scheduler.reminders import resume_reminders
from echo_agent.scheduler.service import ScheduledJob, JobStatus
from echo_agent.agent.turn_run_store import TurnRunStore
from echo_agent.bus.events import ContentBlock, ContentType, InboundEvent
from echo_agent.bus.queue import MessageBus
from echo_agent.channels.base import SendResult
from echo_agent.channels.manager import ChannelManager
from echo_agent.channels.weixin import WeixinChannel
from echo_agent.config.loader import load_config
from echo_agent.config.schema import ChannelsConfig, WeixinChannelConfig
from echo_agent.scheduler.delivery import build_scheduled_job_handler
from echo_agent.scheduler.output import ScheduledOutputs
from echo_agent.scheduler.reminders import bind_pending_attachments
from echo_agent.scheduler.service import Scheduler, TriggerKind
from echo_agent.security.tool_policy import tool_policy_decision
from echo_agent.storage.sqlite import SQLiteBackend
from echo_agent.tools import ToolExecutionContext


async def validate(workspace: Path, pdf: Path | None) -> dict:
    incoming = workspace / "cache-without-extension"
    if pdf is not None:
        shutil.copyfile(pdf, incoming)
    else:
        import pymupdf
        with pymupdf.open() as document:
            document.new_page().insert_text((72, 72), "Original resume smoke test")
            document.save(incoming)
    before = hashlib.sha256(incoming.read_bytes()).hexdigest()
    extracted = extract(incoming)
    assert extracted.text and extracted.meta["format"] == "pdf"
    builder = ContextBuilder(workspace)
    bus = MessageBus()
    storage = SQLiteBackend(workspace / "runtime.db")
    await storage.initialize()
    ledger = TurnRunStore(storage)
    bus.set_turn_run_store(ledger)
    scheduler = Scheduler(workspace / "data" / "scheduler.json", on_job=build_scheduled_job_handler(bus))
    outputs = ScheduledOutputs(bus, scheduler, ledger, workspace)
    bus.scheduled_outputs = outputs
    manager = ChannelManager(ChannelsConfig(), bus)
    channel = WeixinChannel(WeixinChannelConfig(account_id="smoke-bot", token="smoke-only",
                           data_dir=str(workspace / "weixin")), bus)
    channel._send_session = MagicMock()
    channel._running = True
    channel._token_store.set("smoke-bot", "smoke-user", "smoke-context")
    manager._channels["weixin"] = channel
    bus.channel_lookup = manager.get_channel
    transport = AsyncMock(return_value={"message_id": 123})
    channel._send_file = AsyncMock(side_effect=[
        SendResult(False, error="simulated context outage", recovery_action="send_message"),
        SendResult(True, message_id="confirmed-file"),
    ])
    tool = CronjobTool(scheduler, builder.attachments)
    tool._channel_lookup = manager.get_channel
    ctx = ToolExecutionContext(session_key="weixin:smoke-user", user_id="smoke-user",
                               channel="weixin", chat_id="smoke-user", inbound_event_id="smoke-request")
    try:
        created = await tool.execute({"action": "create", "name": "单次面试提醒",
            "at": (datetime.now(timezone(timedelta(hours=8))) + timedelta(days=2)).isoformat(),
            "delivery_text": "15:00 面试；腾讯会议信息。", "await_attachment": True}, ctx)
        assert created.success and "附件待关联" in created.output
        event = InboundEvent(channel="weixin", sender_id="smoke-user", chat_id="smoke-user", content=[
            ContentBlock(type=ContentType.FILE, url=str(incoming), metadata={"name": "面试简历.pdf"})])
        resolved = await builder.prepare_event_media(event)
        record = builder.attachments.get(ctx.session_key, resolved[0]["attachment_id"])
        incoming.unlink()
        assert "已加入提醒" in bind_pending_attachments(scheduler, builder.attachments, ctx.session_key, [record["id"]])
        job = scheduler.get_job(created.metadata["job_id"])
        assert job.trigger == TriggerKind.ONCE and job.delete_after_run
        with patch("echo_agent.channels.weixin._send_message", transport):
            assert await scheduler.trigger_job(job.id)
            assert job.last_status == "delivery_pending"
            await outputs._recover("weixin", "smoke-user")
        assert job.last_status == "delivery_recovered"
        assert transport.await_count == 1 and channel._send_file.await_count == 2
        assert hashlib.sha256(builder.attachments.path(record).read_bytes()).hexdigest() == before
        assert builder.attachments.path(record).name == "面试简历.pdf"
        config = load_config(overrides={"tools": {"profile": "coding"}, "security": {"profile": "daemon"}})
        assert all(tool_policy_decision(config, name)[0] for name in ("attachment_list", "read_document", "send_file"))
        # Exercise the review fixes against the installed package as well.
        text_file = workspace / "signature.txt"
        text_file.write_text("文档中引用 %PDF-1.7 只是普通文字")
        assert (await ReadFileTool(str(workspace)).execute({"path": str(text_file)})).success
        spec = {"action": "create", "name": "同名任务", "at":
                (datetime.now(timezone.utc) + timedelta(days=3)).isoformat(), "delivery_text": "甲"}
        first = await tool.execute(spec, ctx)
        second = await tool.execute(dict(spec, delivery_text="乙"), ctx)
        retry = await tool.execute(spec, ctx)
        assert first.metadata["job_id"] != second.metadata["job_id"] == scheduler.list_jobs()[-1].id
        assert first.metadata["job_id"] == retry.metadata["job_id"]
        protected = scheduler.get_job(first.metadata["job_id"])
        protected.payload["attachment_ids"] = [record["id"]]
        scheduler.save_state()
        try:
            builder.attachments.delete(ctx.session_key, record["id"])
        except ValueError:
            pass
        else:
            raise AssertionError("An active reminder's original was not protected")
        privileged = scheduler.add_job(ScheduledJob(cron_expr="0 9 * * *", payload={
            "command": "write test", "source_session_key": ctx.session_key}))
        privileged.authorization = grant(privileged, operator="smoke", source="test")
        assert (await tool.execute({"action": "update", "job_id": privileged.id, "name": "rename"}, ctx)).success
        assert verify(privileged)
        cancelled = scheduler.add_job(ScheduledJob(enabled=False, status=JobStatus.PAUSED,
            payload={"delivery_mode": "reminder", "delivery_text": "must not send", "source_session_key": ctx.session_key},
            run_history=[{"run_id": "cancelled", "status": "interrupted"}]))
        with patch("echo_agent.channels.weixin._send_message", transport):
            await resume_reminders(outputs)
        assert transport.await_count == 1 and cancelled.run_history[0]["status"] == "interrupted"
        return {"ok": True, "checks": ["extensionless_pdf", "durable_original", "late_binding", "single_occurrence",
            "channel_manager", "wechat_transport", "partial_recovery", "no_duplicate_text", "tool_policy", "quoted_pdf_text", "multi_reminder_identity", "pinned_original", "rename_preserves_consent", "paused_recovery_blocked"],
            "pdf_pages": extracted.unit_count, "extracted_chars": len(extracted.text), "external_sends": 0}
    finally:
        await outputs.stop()
        await scheduler.stop()
        await storage.close()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--pdf", type=Path)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="echo-attachment-smoke-") as directory:
        print(json.dumps(asyncio.run(validate(Path(directory), args.pdf)), ensure_ascii=False))
