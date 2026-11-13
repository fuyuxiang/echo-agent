"""Original-file reminders across ordering, restarts and real channel checkpoints."""
import asyncio
from datetime import datetime, timedelta, timezone
from unittest.mock import AsyncMock, MagicMock

import pytest
import pytest_asyncio

from echo_agent.agent.context import ContextBuilder
from echo_agent.agent.media.document_extract import extract
from echo_agent.agent.tools.cronjob import CronjobTool
from echo_agent.agent.tools.document import ReadDocumentTool
from echo_agent.agent.tools.filesystem import ReadFileTool
from echo_agent.agent.turn_run_store import TurnRunStore
from echo_agent.attachments import AttachmentStore
from echo_agent.bus.events import ContentBlock, ContentType, InboundEvent
from echo_agent.bus.queue import MessageBus
from echo_agent.channels.base import SendResult
from echo_agent.channels.weixin import WeixinChannel
from echo_agent.channels.manager import ChannelManager
from echo_agent.config.schema import WeixinChannelConfig, ChannelsConfig
from echo_agent.scheduler.delivery import build_scheduled_job_handler
from echo_agent.scheduler.output import ScheduledOutputs
from echo_agent.scheduler.reminders import bind_pending_attachments, resume_reminders
from echo_agent.scheduler.service import JobStatus, Scheduler, TriggerKind
from echo_agent.storage.sqlite import SQLiteBackend
from echo_agent.tools import ToolExecutionContext


def context(session="weixin:user", event="request"):
    return ToolExecutionContext(session_key=session, channel="weixin", chat_id="user", user_id="user", inbound_event_id=event)


def future():
    return (datetime.now(timezone(timedelta(hours=8))) + timedelta(days=2)).isoformat()


def original(tmp_path, store, event="pdf", session="weixin:user", valid=True):
    file = tmp_path / ("cache-" + event)
    if valid:
        fitz = pytest.importorskip("pymupdf")
        doc = fitz.open()
        doc.new_page().insert_text((72, 72), "Original resume content")
        doc.save(file)
        doc.close()
    else:
        file.write_bytes(b"%PDF-1.7 broken document")
    return store.save(session, event, 0, {"url": str(file), "type": "file", "name": "候选人简历.pdf"})


@pytest_asyncio.fixture
async def runtime(tmp_path, monkeypatch):
    from types import SimpleNamespace
    storage = SQLiteBackend(tmp_path / "runtime.db")
    await storage.initialize()
    ledger, bus = TurnRunStore(storage), MessageBus()
    bus.set_turn_run_store(ledger)
    scheduler = Scheduler(tmp_path / "scheduler.json", on_job=build_scheduled_job_handler(bus))
    store = AttachmentStore(tmp_path)
    outputs = ScheduledOutputs(bus, scheduler, ledger, tmp_path)
    bus.scheduled_outputs = outputs
    channel = WeixinChannel(WeixinChannelConfig(account_id="bot", token="secret", data_dir=str(tmp_path / "weixin")), bus)
    channel._send_session = MagicMock()
    channel._running = True
    channel._token_store.set("bot", "user", "context")
    channel._send_file = AsyncMock(return_value=SendResult(True, message_id="file-receipt"))
    transport = AsyncMock(return_value={"message_id": 123})
    monkeypatch.setattr("echo_agent.channels.weixin._send_message", transport)
    manager = ChannelManager(ChannelsConfig(), bus)
    manager._channels["weixin"] = channel
    bus.channel_lookup = manager.get_channel
    tool = CronjobTool(scheduler, store)
    try:
        yield SimpleNamespace(**locals())
    finally:
        await outputs.stop()
        await scheduler.stop()
        await storage.close()


@pytest.mark.asyncio
async def test_extensionless_pdf_extracts_and_survives_cache_deletion(tmp_path):
    store = AttachmentStore(tmp_path)
    record = original(tmp_path, store)
    cache = tmp_path / "cache-pdf"
    assert "Original resume" in extract(cache).text
    cache.unlink()
    restored = AttachmentStore(tmp_path).get("weixin:user", record["id"])
    assert store.path(restored).suffix == ".pdf"
    result = await ReadDocumentTool(str(tmp_path)).execute({"attachment_id": record["id"]}, context())
    assert result.success and "Original resume" in result.output
    forbidden = await ReadDocumentTool(str(tmp_path)).execute({"attachment_id": record["id"]}, context("weixin:other"))
    assert not forbidden.success
    binary = await ReadFileTool(str(tmp_path)).execute({"path": str(store.path(record))}, context())
    assert not binary.success and "read_document" in binary.error


@pytest.mark.asyncio
async def test_original_is_saved_even_if_extraction_disabled_or_broken(tmp_path):
    builder = ContextBuilder(tmp_path, doc_enabled=False)
    record = original(tmp_path, builder.attachments, valid=False)
    event = InboundEvent(channel="weixin", sender_id="user", chat_id="user", content=[
        ContentBlock(type=ContentType.FILE, url=str(builder.attachments.path(record)), metadata={"name": record["name"]})])
    items = await builder.prepare_event_media(event)
    assert items[0]["attachment_id"]
    assert "extracted_text" not in items[0]
    again = await builder.prepare_event_media(event)
    assert again is items


@pytest.mark.asyncio
async def test_file_first_binds_recent_original_and_sends_without_model(runtime):
    r = runtime
    record = original(r.tmp_path, r.store)
    result = await r.tool.execute({"action": "create", "name": "面试", "at": future(),
        "delivery_text": "今天15:00面试，腾讯会议 697-481-686", "await_attachment": True}, context())
    assert result.success
    job = r.scheduler.get_job(result.metadata["job_id"])
    assert job.trigger == TriggerKind.ONCE and job.delete_after_run
    assert job.payload["attachment_ids"] == [record["id"]]
    assert await r.scheduler.trigger_job(job.id)
    assert r.bus.pending_inbound == 0
    assert job.status == JobStatus.COMPLETED and job.last_status == "completed"
    assert job.run_history[-1]["delivery_status"] == "delivered"
    r.transport.assert_awaited_once()
    assert "腾讯会议" in r.transport.call_args.kwargs["text"]
    r.channel._send_file.assert_awaited_once()
    # Original name preserved at the real channel upload boundary.
    assert r.channel._send_file.call_args.args[1].endswith("候选人简历.pdf")
    assert not await r.scheduler.trigger_job(job.id)


@pytest.mark.asyncio
async def test_request_first_late_attachment_after_restart_is_bound(runtime):
    r = runtime
    result = await r.tool.execute({"action": "create", "name": "面试", "at": future(),
        "delivery_text": "面试提醒", "await_attachment": True}, context())
    job_id = result.metadata["job_id"]
    assert "附件待关联" in result.output
    restored = Scheduler(r.tmp_path / "scheduler.json", on_job=build_scheduled_job_handler(r.bus))
    record = original(r.tmp_path, r.store)
    receipt = bind_pending_attachments(restored, r.store, "weixin:user", [record["id"]])
    assert "已加入提醒" in receipt
    assert restored.get_job(job_id).payload["attachment_ids"] == [record["id"]]
    assert not bind_pending_attachments(restored, r.store, "weixin:user", [record["id"]])


@pytest.mark.asyncio
async def test_multiple_files_or_pending_tasks_require_disambiguation(runtime):
    r = runtime
    first = original(r.tmp_path, r.store, event="one")
    second = original(r.tmp_path, r.store, event="two")
    result = await r.tool.execute({"action": "create", "at": future(), "delivery_text": "提醒", "await_attachment": True}, context())
    assert not result.success and "多个" in result.error
    # Other conversations cannot bind this file, even knowing its ID.
    result = await r.tool.execute({"action": "create", "at": future(), "delivery_text": "提醒",
        "attachment_ids": [first["id"]]}, context("weixin:other"))
    assert not result.success
    assert not bind_pending_attachments(r.scheduler, r.store, "weixin:user", [first["id"], second["id"]])


@pytest.mark.asyncio
async def test_text_receipt_is_not_resent_when_file_recovers(runtime):
    r = runtime
    record = original(r.tmp_path, r.store)
    r.channel._send_file.return_value = SendResult(False, error="context expired", recovery_action="send_message")
    result = await r.tool.execute({"action": "create", "at": future(), "delivery_text": "面试提醒",
        "attachment_ids": [record["id"]]}, context())
    job = r.scheduler.get_job(result.metadata["job_id"])
    await r.scheduler.trigger_job(job.id)
    assert job.last_status == "delivery_pending"
    r.transport.assert_awaited_once()
    assert len(await r.ledger.waiting_outbound("weixin", "user")) == 1
    r.channel._send_file.return_value = SendResult(True, message_id="file-recovered")
    await r.outputs._recover("weixin", "user")
    assert job.run_history[-1]["delivery_status"] == "delivered"
    assert job.last_status == "delivery_recovered"
    r.transport.assert_awaited_once()
    assert r.channel._send_file.await_count == 2


@pytest.mark.asyncio
async def test_dispatch_crash_recovers_unsent_file_without_duplicate_text(runtime):
    r = runtime
    record = original(r.tmp_path, r.store)
    result = await r.tool.execute({"action": "create", "at": future(), "delivery_text": "面试提醒",
        "attachment_ids": [record["id"]]}, context())
    job = r.scheduler.get_job(result.metadata["job_id"])
    real_publish = r.bus.publish_outbound
    async def crash_on_file(event):
        if event.content[0].type == ContentType.FILE:
            raise asyncio.CancelledError
        return await real_publish(event)
    r.bus.publish_outbound = crash_on_file
    with pytest.raises(asyncio.CancelledError):
        await r.scheduler.trigger_job(job.id)
    r.bus.publish_outbound = real_publish
    assert job.run_history[-1]["status"] == "interrupted"
    await resume_reminders(r.outputs)
    assert job.run_history[-1]["delivery_status"] == "delivered"
    r.transport.assert_awaited_once()
    r.channel._send_file.assert_awaited_once()


@pytest.mark.asyncio
async def test_yearless_fixed_date_is_single_and_update_uses_same_id(runtime):
    r = runtime
    result = await r.tool.execute({"action": "create", "name": "面试", "schedule": "0 14 15 11 *", "delivery_text": "提醒"}, context())
    job = r.scheduler.get_job(result.metadata["job_id"])
    assert job.trigger == TriggerKind.ONCE
    updated = await r.tool.execute({"action": "update", "job_id": job.id, "at": future(), "delivery_text": "新会议"}, context())
    assert updated.success and len(r.scheduler.list_jobs()) == 1
    assert job.payload["delivery_text"] == "新会议"
    assert not (await r.tool.execute({"action": "update", "job_id": job.id, "delivery_text": "hijack"}, context("weixin:other"))).success


@pytest.mark.asyncio
async def test_unreadable_pdf_still_delivers_original(runtime):
    r = runtime
    record = original(r.tmp_path, r.store, valid=False)
    assert not extract(r.store.path(record)).text
    result = await r.tool.execute({"action": "create", "at": future(), "delivery_text": "提醒",
        "attachment_ids": [record["id"]]}, context())
    job = r.scheduler.get_job(result.metadata["job_id"])
    await r.scheduler.trigger_job(job.id)
    assert job.last_status == "completed"
    r.channel._send_file.assert_awaited_once()


@pytest.mark.asyncio
async def test_duplicate_tool_call_does_not_create_duplicate_reminder(runtime):
    r = runtime
    params = {"action": "create", "name": "面试", "at": future(), "delivery_text": "提醒"}
    first = await r.tool.execute(params, context())
    second = await r.tool.execute(params, context())
    assert first.metadata["job_id"] == second.metadata["job_id"]
    assert len(r.scheduler.list_jobs()) == 1


@pytest.mark.asyncio
async def test_late_pdf_through_agent_loop_confirms_without_calling_model(tmp_path):
    from echo_agent.agent.loop import AgentLoop
    from echo_agent.config.loader import load_config
    from echo_agent.models.provider import LLMProvider

    class NoInference(LLMProvider):
        async def chat(self, *args, **kwargs):
            raise AssertionError("Binding an original must not require inference")
        def get_default_model(self):
            return "test"

    bus = MessageBus()
    scheduler = Scheduler(tmp_path / "scheduler.json")
    store = AttachmentStore(tmp_path)
    tool = CronjobTool(scheduler, store)
    result = await tool.execute({"action": "create", "at": future(), "delivery_text": "面试提醒",
                                 "await_attachment": True}, context())
    assert result.success
    record = original(tmp_path, store)
    config = load_config(overrides={"workspace": str(tmp_path), "memory": {"enabled": False}})
    agent = AgentLoop(bus=bus, config=config, workspace=tmp_path, provider=NoInference(), scheduler=scheduler)
    event = InboundEvent(channel="weixin", sender_id="user", chat_id="user", content=[
        ContentBlock(type=ContentType.FILE, url=str(store.path(record)), metadata={"name": record["name"]})])
    response = await agent._process_event(event, "binding-trace")
    assert "已加入提醒" in response.response_text
    session = await agent.sessions.get_or_create("weixin:user")
    assert session.messages[-2]["media_refs"][0]["attachment_id"]
    assert session.messages[-2]["content"]


@pytest.mark.asyncio
async def test_rest_single_reminder_create_edit_preserves_original(runtime):
    import json
    from types import SimpleNamespace
    from echo_agent.gateway.api.cron_api import CronAPI
    r = runtime
    record = original(r.tmp_path, r.store)
    server = SimpleNamespace(_require_admin_token=lambda *a, **k: None,
        _agent_loop=SimpleNamespace(scheduler=r.scheduler), _workspace=r.tmp_path)
    api = CronAPI(server)
    payload = {"command": "面试提醒", "delivery_text": "面试提醒", "delivery_mode": "reminder",
               "source_session_key": "weixin:user", "attachment_ids": [record["id"]]}
    request = SimpleNamespace(json=AsyncMock(return_value={"name": "面试", "trigger": "once", "at": future(), "payload": payload}))
    response = await api.create_job(request)
    assert response.status == 201
    job_id = json.loads(response.text)["id"]
    job = r.scheduler.get_job(job_id)
    assert job.trigger == TriggerKind.ONCE
    request = SimpleNamespace(match_info={"id": job_id}, json=AsyncMock(return_value={"name": "改名", "payload": {"command": "新的会议信息"}}))
    response = await api.update_job(request)
    assert response.status == 200
    assert job.payload["delivery_text"] == "新的会议信息"
    assert job.payload["attachment_ids"] == [record["id"]]
    assert job.trigger == TriggerKind.ONCE


def test_ciphertext_cache_is_immutable_and_repeat_decryption_is_idempotent(tmp_path):
    import base64
    from echo_agent.channels.weixin import _aes128_ecb_encrypt
    key = b"0123456789abcdef"
    encoded = base64.b64encode(key).decode()
    plaintext = b"%PDF-1.7 original bytes"
    ciphertext = _aes128_ecb_encrypt(plaintext, key)
    path = tmp_path / "encrypted-cache"
    path.write_bytes(ciphertext)
    first = ContextBuilder._decrypt_media_file(path, encoded)
    second = ContextBuilder._decrypt_media_file(path, encoded)
    assert first == second and first.read_bytes() == plaintext
    assert path.read_bytes() == ciphertext


@pytest.mark.asyncio
async def test_attachment_tools_are_exposed_by_coding_profile(tmp_path):
    from echo_agent.config.loader import load_config
    from echo_agent.security.tool_policy import tool_policy_decision
    config = load_config(overrides={"tools": {"profile": "coding"}})
    for name in ("attachment_list", "read_document", "send_file"):
        assert tool_policy_decision(config, name)[0], name


@pytest.mark.asyncio
async def test_abrupt_restart_cannot_create_second_execution_for_single_reminder(runtime):
    r = runtime
    record = original(r.tmp_path, r.store)
    result = await r.tool.execute({"action": "create", "at": future(), "delivery_text": "提醒",
                                  "attachment_ids": [record["id"]]}, context())
    job = r.scheduler.get_job(result.metadata["job_id"])
    await r.scheduler.trigger_job(job.id)
    # Simulate the persisted snapshot from before _run_job's finally ran.
    job.status = JobStatus.ACTIVE
    job.run_history[-1]["status"] = "dispatching"
    r.scheduler.save_state()
    restored = Scheduler(r.tmp_path / "scheduler.json", on_job=build_scheduled_job_handler(r.bus))
    await restored.start()
    try:
        restored_job = restored.get_job(job.id)
        assert restored_job.status == JobStatus.COMPLETED
        assert restored_job.next_run_ms is None
        assert restored_job.run_count == 1
        r.outputs.scheduler = restored
        await resume_reminders(r.outputs)
        r.transport.assert_awaited_once()
        r.channel._send_file.assert_awaited_once()
        assert restored_job.run_history[-1]["status"] == "completed"
    finally:
        await restored.stop()


def test_manifest_filename_cannot_overwrite_original_or_escape_scope(tmp_path):
    store = AttachmentStore(tmp_path)
    source = tmp_path / "input"
    source.write_bytes(b'{"original": true}')
    record = store.save("weixin:user", "manifest", 0, {"url": str(source), "name": "../../manifest.json", "type": "file"})
    assert store.get("weixin:user", record["id"])["name"] == "manifest.json"
    assert store.path(record).read_bytes() == source.read_bytes()


@pytest.mark.asyncio
async def test_fixed_reminders_do_not_report_missing_exec_authorization(runtime, monkeypatch):
    r = runtime
    result = await r.tool.execute({"action": "create", "at": future(), "delivery_text": "提醒"}, context())
    assert result.success
    warning = MagicMock()
    monkeypatch.setattr("echo_agent.scheduler.service.logger.warning", warning)
    r.scheduler._warn_unauthorized_jobs()
    warning.assert_not_called()
