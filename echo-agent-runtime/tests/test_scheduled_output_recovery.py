import asyncio
import json
from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest
import pytest_asyncio

from echo_agent.agent.turn_run_store import TurnRunStore
from echo_agent.bus.events import ContentBlock, ContentType, OutboundEvent
from echo_agent.bus.queue import MessageBus
from echo_agent.channels.base import SendResult
from echo_agent.channels.weixin import WeixinChannel
from echo_agent.channels.manager import ChannelManager
from echo_agent.config.schema import WeixinChannelConfig
from echo_agent.gateway.api.deliveries import DeliveriesAPI
from echo_agent.scheduler.delivery import build_scheduled_job_handler
from echo_agent.scheduler.output import ScheduledOutputs
from echo_agent.scheduler.service import ScheduledJob, Scheduler, TriggerKind
from echo_agent.storage.sqlite import SQLiteBackend


@pytest_asyncio.fixture
async def runtime(tmp_path, monkeypatch):
    storage = SQLiteBackend(tmp_path / "runtime.db")
    await storage.initialize()
    store, bus = TurnRunStore(storage), MessageBus()
    bus.set_turn_run_store(store)
    scheduler = Scheduler(tmp_path / "scheduler.json", on_job=build_scheduled_job_handler(bus))
    outputs = ScheduledOutputs(bus, scheduler, store, tmp_path)
    bus.scheduled_outputs = outputs
    channel = WeixinChannel(WeixinChannelConfig(
        account_id="test@im.bot", token="secret", data_dir=str(tmp_path / "weixin")), bus)
    channel._send_session = MagicMock()
    channel._handle_message = AsyncMock()
    channel._running = True
    channel._send_file = AsyncMock(return_value=SendResult(True, message_id="file-receipt"))
    transport = AsyncMock(return_value={"message_id": 123})
    monkeypatch.setattr("echo_agent.channels.weixin._send_message", transport)
    bus.subscribe_outbound("weixin", channel.send)
    job = scheduler.add_job(ScheduledJob(name="Daily report", trigger=TriggerKind.CRON, cron_expr="30 6 * * *",
        payload={"command": "Generate a report", "deliver_channel": "weixin", "deliver_chat_id": "user",
                 "report_history_path": "reports/history.json"}))
    assert await scheduler.trigger_job(job.id)
    source = await bus._inbound_queue.get()
    report = tmp_path / "report.md"
    report.write_text("A real, finalized report")

    def outbound(caption="Daily report", *, artifact=True):
        event = OutboundEvent(channel="weixin", chat_id="user", content=[ContentBlock(type=ContentType.TEXT, text=caption)])
        event.metadata["_inbound_event_id"] = source.event_id
        if artifact:
            event.metadata.update(_tool_delivery=True, _artifact_id="artifact-one")
            event.content.append(ContentBlock(type=ContentType.FILE, url=str(report)))
        return event

    try:
        yield SimpleNamespace(**locals())
    finally:
        await outputs.stop()
        await storage.close()


async def refresh(runtime):
    await runtime.channel._process_message({"from_user_id": "user", "message_id": "fresh-message",
        "context_token": "fresh-context", "item_list": [{"type": 1, "text_item": {"text": "你好"}}]})
    await asyncio.wait_for(asyncio.gather(*list(runtime.outputs._recoveries.values())), 3)


@pytest.mark.asyncio
async def test_missing_context_retains_report_and_fresh_message_recovers_exactly_once(runtime):
    r = runtime
    event = r.outbound()
    result = await r.bus.publish_outbound(event)
    assert not result.ok and result.detail["error_code"] == "weixin_context_required"
    r.transport.assert_not_awaited()
    await r.scheduler.record_run_outcome(r.job.id, "error", "delivery failed", run_id=r.source.metadata["run_id"])
    await r.outputs.finished(r.source)
    assert r.job.last_status == "delivery_pending"
    await refresh(r)
    assert await r.store.list_outbound() == []
    assert r.job.last_status == "delivery_recovered"
    assert r.job.run_count == 1  # Recovery never re-executes the agent/tools.
    r.channel._send_file.assert_awaited_once()
    await r.outputs._recover("weixin", "user")
    r.channel._send_file.assert_awaited_once()
    history = json.loads((r.tmp_path / "reports/history.json").read_text())
    assert history["reports"][0]["status"] == "success"
    assert history["reports"][0]["pushed_at"]


@pytest.mark.asyncio
async def test_upstream_rejection_stops_retries_and_caption_changes_do_not_duplicate_report(runtime):
    r = runtime
    r.channel._token_store.set("test@im.bot", "user", "old-context")
    r.transport.return_value = {"ret": -2, "errmsg": "prepare failed"}
    first = r.outbound()
    result = await r.bus.publish_outbound(first)
    assert result.detail["recovery_action"] == "send_message"
    second = r.outbound("Another caption chosen by the model")
    result = await r.bus.publish_outbound(second)
    assert not result.ok and first.event_id == second.event_id
    r.transport.assert_awaited_once()
    assert len(await r.store.list_outbound()) == 1
    r.transport.return_value = {"message_id": 456}
    await refresh(r)
    assert r.transport.await_count == 2
    assert await r.store.list_outbound() == []


@pytest.mark.asyncio
async def test_unknown_send_is_never_automatically_replayed(runtime):
    r = runtime
    r.channel._token_store.set("test@im.bot", "user", "old-context")
    r.transport.side_effect = asyncio.TimeoutError()
    event = r.outbound()
    result = await r.bus.publish_outbound(event)
    assert result.detail["delivery_unknown"]
    await refresh(r)
    r.transport.assert_awaited_once()
    records = await r.store.list_outbound()
    assert len(records) == 1 and records[0]["status"] == "unknown"


@pytest.mark.asyncio
async def test_receipts_override_premature_model_history_and_failure_summary_is_not_replayed(runtime):
    r = runtime
    await r.bus.publish_outbound(r.outbound())
    path = r.tmp_path / "reports/history.json"
    data = json.loads(path.read_text())
    day = data["reports"][0]["date"]
    data["reports"][0].update(status="success", pushed_at="invented")
    data["technologies"] = [{"name": "paper", "last_pushed": day}]
    path.write_text(json.dumps(data))
    await r.outputs.finished(r.source)
    data = json.loads(path.read_text())
    assert data["reports"][0]["status"] == "pending_delivery"
    assert data["reports"][0]["pushed_at"] is None
    assert data["technologies"][0]["last_pushed"] is None
    notice = r.outbound("Delivery failed. Please continue.", artifact=False)
    notice.delivery_origin = r.source
    notice.metadata["_turn_status"] = "incomplete"
    await r.bus.publish_outbound(notice)
    assert len(await r.store.list_outbound()) == 1
    await refresh(r)
    assert r.channel._send_file.await_count == 1
    assert (await r.store._storage.fetch_sql("SELECT status FROM outbound_messages WHERE event_id=?", (notice.event_id,)))[0]["status"] != "delivered"


@pytest.mark.asyncio
async def test_recovery_survives_process_restart_and_does_not_cross_chat_boundaries(runtime):
    r = runtime
    await r.bus.publish_outbound(r.outbound())
    await r.outputs.finished(r.source)
    r.bus.scheduled_outputs = ScheduledOutputs(r.bus, r.scheduler, r.store, r.tmp_path)
    await r.bus.scheduled_outputs._recover("weixin", "different-user")
    r.transport.assert_not_awaited()
    r.channel._token_store.set("test@im.bot", "user", "fresh-context")
    await r.bus.scheduled_outputs._recover("weixin", "user")
    assert await r.store.list_outbound() == []
    r.channel._send_file.assert_awaited_once()


@pytest.mark.asyncio
async def test_dashboard_explains_required_action_and_prevents_useless_retry(runtime):
    r = runtime
    event = r.outbound()
    await r.bus.publish_outbound(event)
    server = SimpleNamespace(_agent_loop=SimpleNamespace(_turn_runs=r.store), _bus=r.bus,
        channel_manager=SimpleNamespace(get_channel=lambda _: r.channel),
        _require_admin_token=lambda *a, **kw: None)
    api = DeliveriesAPI(server)
    request = MagicMock(query={})
    row = json.loads((await api.list_messages(request)).text)["messages"][0]
    assert row["recovery_action"] == "send_message" and row["auto_retry"] and not row["can_retry"]
    request.match_info = {"id": event.event_id}
    request.json = AsyncMock(return_value={"action": "retry"})
    response = await api.recover(request)
    assert response.status == 409
    assert json.loads(response.text)["error_code"] == "weixin_context_required"
    r.transport.assert_not_awaited()


@pytest.mark.asyncio
async def test_late_recovery_does_not_overwrite_a_newer_run(runtime):
    r = runtime
    await r.bus.publish_outbound(r.outbound())
    old_run_id = r.source.metadata["run_id"]
    await r.scheduler.record_run_outcome(r.job.id, "error", "delivery failed", run_id=old_run_id)
    await r.outputs.finished(r.source)
    assert await r.scheduler.trigger_job(r.job.id)
    new_source = await r.bus._inbound_queue.get()
    await r.scheduler.record_run_outcome(r.job.id, "incomplete", "new run incomplete", run_id=new_source.metadata["run_id"])
    await refresh(r)
    assert r.job.last_status == "incomplete" and r.job.last_error == "new run incomplete"
    assert r.job.run_history[0]["status"] == "delivery_recovered"


@pytest.mark.asyncio
async def test_startup_recovery_uses_saved_context_and_receipts(runtime):
    r = runtime
    await r.bus.publish_outbound(r.outbound())
    r.channel._token_store.set("test@im.bot", "user", "fresh-context")
    outputs = ScheduledOutputs(r.bus, r.scheduler, r.store, r.tmp_path)
    r.bus.scheduled_outputs = outputs
    await outputs.resume_waiting(lambda _: r.channel)
    await asyncio.wait_for(asyncio.gather(*list(outputs._recoveries.values())), 3)
    r.channel._send_file.assert_awaited_once()
    assert await r.store.list_outbound() == []


@pytest.mark.asyncio
async def test_real_channel_manager_preserves_durable_report_identity_across_restart(runtime):
    from echo_agent.config.schema import ChannelsConfig
    r = runtime
    manager = ChannelManager(ChannelsConfig(), r.bus)
    manager._channels["weixin"] = r.channel
    r.bus.subscribe_outbound_global(manager._filter_and_dispatch)
    event = r.outbound()
    assert not (await r.bus.publish_outbound(event)).ok
    r.bus.scheduled_outputs = ScheduledOutputs(r.bus, r.scheduler, r.store, r.tmp_path)
    r.channel._token_store.set("test@im.bot", "user", "fresh")
    await r.bus.scheduled_outputs._recover("weixin", "user")
    assert await r.store.list_outbound() == []
    assert r.job.run_history[-1]["deliveries"][0]["artifact_id"] == "artifact-one"
    r.channel._send_file.assert_awaited_once()


@pytest.mark.asyncio
async def test_saved_report_download_stays_inside_workspace_and_requires_admin(runtime):
    r = runtime
    event = r.outbound()
    await r.bus.publish_outbound(event)
    server = SimpleNamespace(_workspace=r.tmp_path, _agent_loop=SimpleNamespace(_turn_runs=r.store),
        _require_admin_token=lambda *a, **kw: None)
    api = DeliveriesAPI(server)
    request = MagicMock(match_info={"id": event.event_id, "index": "0"})
    response = await api.download_file(request)
    assert response.status == 200 and "attachment" in response.headers["Content-Disposition"]
    assert response.headers["Cache-Control"] == "no-store"
    request.match_info["index"] = "-1"
    assert (await api.download_file(request)).status == 404
    assert api._local_file("https://example.com/report.md") is None
    assert api._local_file("/etc/passwd") is None
    link = r.tmp_path / "outside"
    link.symlink_to("/etc/passwd")
    assert api._local_file(str(link)) is None
    from aiohttp import web
    server._require_admin_token = lambda *a, **kw: web.json_response({"error": "admin only"}, status=403)
    assert (await api.download_file(request)).status == 403


@pytest.mark.asyncio
async def test_recovery_completes_all_text_fallback_parts_and_artifact_manifest(runtime):
    from echo_agent.artifacts.store import ArtifactStore
    from echo_agent.agent.tools.artifact import ArtifactDeliverTool
    from echo_agent.tools import ToolExecutionContext

    r = runtime
    store = ArtifactStore(r.tmp_path, "artifacts", max_chunk_chars=10000, max_artifact_mb=1, allowed_extensions=[".md"])
    created = await store.create(r.source.session_key, filename="long-report.md")
    artifact_id = created["artifact_id"]
    content = "# Report\n\n" + "All paragraphs must arrive.\n" * 25
    await store.append(r.source.session_key, artifact_id, sequence=0, content=content)
    await store.finalize(r.source.session_key, artifact_id)
    r.channel.supports_files = False
    tool = ArtifactDeliverTool(store, r.bus.publish_outbound, lambda _: r.channel, text_fallback_chunk_chars=100)
    r.outputs.artifact_deliver = tool
    ctx = ToolExecutionContext(session_key=r.source.session_key, channel="weixin", chat_id="user", inbound_event_id=r.source.event_id)
    assert not (await tool.execute({"artifact_id": artifact_id}, ctx)).success
    await r.scheduler.record_run_outcome(r.job.id, "error", "delivery failed", run_id=r.source.metadata["run_id"])
    # Simulate a changed runtime default after a restart. Recovery must keep
    # the original part boundaries and immutable delivery identity.
    tool._text_fallback_chunk_chars = 200
    await refresh(r)
    assert r.job.run_history[-1]["delivery_status"] == "delivered"
    assert r.transport.await_count == 8
    _, manifest = await store.finalized_path(r.source.session_key, artifact_id)
    assert manifest["deliveries"][0]["completed_parts"] == 8
    assert manifest["deliveries"][0]["delivered_at"]
    assert await r.store.list_outbound() == []
