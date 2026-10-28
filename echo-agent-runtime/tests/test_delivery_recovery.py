import asyncio
import json
from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest
import pytest_asyncio

from echo_agent.agent.turn_run_store import TurnRunStore
from echo_agent.bus.events import OutboundEvent
from echo_agent.bus.queue import MessageBus
from echo_agent.channels.base import BaseChannel, SendResult, DeliveryRejected
from echo_agent.gateway.api.deliveries import DeliveriesAPI
from echo_agent.storage.sqlite import SQLiteBackend


class PartsChannel(BaseChannel):
    name = "fake"
    def __init__(self, bus):
        super().__init__(SimpleNamespace(allow_from=[]), bus)
        self._running = True
        self.calls = []
        self.fail = "rejected"
        self.entered, self.release = asyncio.Event(), asyncio.Event()

    async def start(self):
        pass

    async def stop(self):
        pass

    async def send(self, event):
        async def send_part(index, part):
            self.calls.append(part)
            if index == 1 and self.fail:
                failure, self.fail = self.fail, ""
                if failure == "unknown":
                    raise asyncio.TimeoutError()
                if failure == "blocked":
                    self.entered.set()
                    await self.release.wait()
                else:
                    raise DeliveryRejected("refused")
            return SendResult(success=True, message_id=f"receipt-{index}")
        return await self.send_parts(event, event.text.split("\n"), send_part)


@pytest_asyncio.fixture
async def recovery(tmp_path):
    backend = SQLiteBackend(tmp_path / "delivery.db")
    await backend.initialize()
    store, bus = TurnRunStore(backend), MessageBus()
    bus.set_turn_run_store(store)
    channel = PartsChannel(bus)
    bus.subscribe_outbound(channel.name, channel.send)
    server = SimpleNamespace(_agent_loop=SimpleNamespace(_turn_runs=store, restore_delivered_clarification=AsyncMock()),
                             _bus=bus, channel_manager=SimpleNamespace(get_channel=lambda name: channel),
                             _require_admin_token=lambda *args, **kwargs: None)
    try:
        yield DeliveriesAPI(server), store, bus, channel
    finally:
        await backend.close()


def request(event, **body):
    req = MagicMock()
    req.match_info = {"id": event.event_id}
    req.json = AsyncMock(return_value=body)
    return req


@pytest.mark.asyncio
async def test_recovery_reuses_the_saved_message_and_skips_confirmed_parts(recovery):
    api, store, bus, channel = recovery
    event = OutboundEvent.text_reply("fake", "chat", "one\ntwo\nthree", event_id="reply:a_b%")
    assert not (await bus.publish_outbound(event)).ok
    listed = json.loads((await api.list_messages(MagicMock())).text)["messages"]
    assert listed[0]["confirmed_parts"] == 1 and listed[0]["unknown_parts"] == 0
    result = await api.recover(request(event, action="retry"))
    assert result.status == 200
    assert channel.calls == ["one", "two", "two", "three"]
    assert await store.list_outbound() == []


@pytest.mark.asyncio
async def test_recovery_reports_the_receipt_with_the_original_input(recovery):
    from echo_agent.bus.events import InboundEvent
    api, store, bus, _ = recovery
    source = InboundEvent.text_message("fake", "staff", "chat", "原始问题", event_id="native-id")
    event = OutboundEvent.text_reply("fake", "chat", "one\ntwo\nthree", delivery_origin=source)
    await bus.publish_outbound(event)
    restored = await store.get_outbound_event(event.event_id)
    assert restored.delivery_origin == source
    api._server._agent_loop.record_recovered_delivery = AsyncMock()
    assert (await api.recover(request(event, action="retry"))).status == 200
    call = api._server._agent_loop.record_recovered_delivery.call_args
    assert call.args[0].delivery_origin.event_id == "native-id"
    assert call.args[1].ok and len(call.args[1].detail["message_ids"]) == 3


@pytest.mark.asyncio
async def test_an_old_failed_question_cannot_replace_a_newer_question(recovery, tmp_path):
    from echo_agent.bus.events import InboundEvent
    from tests.test_clarification_lifecycle import make_loop
    api, store, bus, channel = recovery
    loop = make_loop(tmp_path, bus)
    loop._turn_runs = store
    api._server._agent_loop = loop
    first = InboundEvent.text_message("fake", "u", "chat", "开始", event_id="first-question")
    await loop._on_inbound(first)
    old = await store.get_outbound_event("reply:first-question")
    assert old.message_kind == "clarification"
    loop.provider.calls = 0
    await loop._on_inbound(InboundEvent.text_message("fake", "u", "chat", "新请求", event_id="new-question"))
    count = len(channel.calls)
    result = await asyncio.wait_for(api.recover(request(old, action="retry")), 2)
    assert result.status == 409 and len(channel.calls) == count


@pytest.mark.asyncio
async def test_failed_question_recovery_restores_binding_without_recursive_locks(recovery, tmp_path):
    from echo_agent.bus.events import InboundEvent
    from tests.test_clarification_lifecycle import make_loop
    api, store, bus, _ = recovery
    loop = make_loop(tmp_path, bus)
    loop._turn_runs = store
    api._server._agent_loop = loop
    await loop._on_inbound(InboundEvent.text_message("fake", "u", "chat", "开始", event_id="restore-question"))
    assert loop.clarify.peek_im_pending("fake:chat", 300, "u") is None
    saved = await store.get_outbound_event("reply:restore-question")
    result = await asyncio.wait_for(api.recover(request(saved, action="retry")), 2)
    assert result.status == 200
    assert loop.clarify.peek_im_pending("fake:chat", 300, "u") is not None


@pytest.mark.asyncio
async def test_unknown_delivery_requires_explicit_verification_before_retry(recovery):
    api, store, bus, channel = recovery
    channel.fail = "unknown"
    event = OutboundEvent.text_reply("fake", "chat", "one\ntwo\nthree")
    result = await bus.publish_outbound(event)
    assert not result.ok and result.detail["delivery_unknown"]
    refused = await api.recover(request(event, action="retry"))
    assert refused.status == 409 and channel.calls == ["one", "two"]
    recovered = await api.recover(request(event, action="retry", confirmed_not_delivered=True))
    assert recovered.status == 200 and channel.calls == ["one", "two", "two", "three"]
    audit = await store._storage.fetch_sql("SELECT action FROM outbound_resolutions")
    assert [row["action"] for row in audit] == ["confirmed_not_delivered"]


@pytest.mark.asyncio
async def test_operator_confirmation_stops_retries_without_sending_again(recovery):
    api, store, bus, channel = recovery
    channel.fail = "unknown"
    event = OutboundEvent.text_reply("fake", "chat", "one\ntwo\nthree")
    await bus.publish_outbound(event)
    assert (await api.recover(request(event, action="confirm_delivery"))).status == 400
    assert (await api.recover(request(event, action="confirm_delivery", confirmed_delivered=True))).status == 200
    assert (await api.recover(request(event, action="retry"))).status == 409
    assert channel.calls == ["one", "two"]
    assert await store.list_outbound() == []


@pytest.mark.asyncio
async def test_live_sends_cannot_be_reset_or_recovered(recovery):
    api, _, bus, channel = recovery
    channel.fail = "blocked"
    event = OutboundEvent.text_reply("fake", "chat", "one\ntwo\nthree")
    sending = asyncio.create_task(bus.publish_outbound(event))
    await asyncio.wait_for(channel.entered.wait(), 5)
    try:
        assert (await api.recover(request(event, action="retry", confirmed_not_delivered=True))).status == 409
    finally:
        channel.release.set()
        assert (await sending).ok


@pytest.mark.asyncio
async def test_recovery_requires_admin_permission(recovery):
    from aiohttp import web
    api, _, _, channel = recovery
    api._server._require_admin_token = lambda *args, **kwargs: web.json_response({"error": "admin required"}, status=403)
    event = OutboundEvent.text_reply("fake", "chat", "one\ntwo")
    assert (await api.list_messages(MagicMock())).status == 403
    assert (await api.recover(request(event, action="retry", confirmed_not_delivered=True))).status == 403
    assert channel.calls == []


@pytest.mark.asyncio
async def test_restart_marks_inflight_delivery_unknown_and_preserves_payload(tmp_path):
    path = tmp_path / "restart.db"
    first = SQLiteBackend(path)
    await first.initialize()
    store = TurnRunStore(first)
    event = OutboundEvent.text_reply("fake", "chat", "one\ntwo", event_id="restart")
    await store.register_outbound(event)
    await store.claim_outbound_part("fake:restart:0", "fingerprint")
    await first.close()
    second = SQLiteBackend(path)
    await second.initialize()
    try:
        restored = TurnRunStore(second)
        await restored.reconcile_orphaned()
        rows = await restored.list_outbound()
        assert rows[0]["status"] == "unknown" and rows[0]["parts"][0]["state"] == "unknown"
        assert (await restored.get_outbound_event("restart")).text == event.text
        with pytest.raises(ValueError, match="confirm"):
            await restored.resolve_outbound("restart")
    finally:
        await second.close()
