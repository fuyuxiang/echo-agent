"""Delivery receipts must reflect what actually happened.

Two shapes of false success this pins:

1. ``GatewayServer._handle_outbound`` returned None unconditionally, which
   ``MessageBus._aggregate`` reads as ACCEPTED — and ``DeliveryResult.ok`` counts
   ACCEPTED as success. So a turn whose FINAL answer reached nobody (client gone,
   no HTTP waiter) still reported success, and the cron run / task that produced
   it was marked complete. The warning in that method already knew the reply had
   been dropped; it just never told the caller.

2. ``send_file`` ignored the DeliveryResult entirely and always returned
   "File sent". Only qqbot and weixin consume structured FILE/IMAGE blocks, so on
   every other channel the caption went out, the attachment was dropped, and the
   model was told the file arrived.

The subtle constraint on (1): ``_handle_outbound`` is a *global* handler that
sees every channel's events. It must stay silent (None) about events it does not
own, or it would vote FAILED on a Telegram delivery that in fact succeeded.
"""

from __future__ import annotations

import asyncio
from unittest.mock import AsyncMock, MagicMock

import pytest

from echo_agent.bus.delivery import DeliveryResult, DeliveryStage
from echo_agent.agent.interrupt_manager import InterruptManager
from echo_agent.bus.events import InboundEvent, OutboundEvent
from echo_agent.bus.queue import MessageBus
from echo_agent.config.schema import (
    GatewayAuthConfig,
    GatewayConfig,
    GatewaySessionPolicyConfig,
)
from echo_agent.gateway.server import GatewayServer


def _gateway() -> GatewayServer:
    session_manager = MagicMock()
    session_manager.get_or_create = AsyncMock(return_value=MagicMock(status="active"))
    config = GatewayConfig(
        enabled=True,
        host="127.0.0.1",
        port=19995,
        auth=GatewayAuthConfig(mode="open"),
        session_policy=GatewaySessionPolicyConfig(mode="none"),
    )
    return GatewayServer(
        config=config,
        bus=MessageBus(),
        channel_manager=MagicMock(),
        session_manager=session_manager,
        workspace=MagicMock(),
        agent_loop=MagicMock(),
    )


def _final(channel: str = "gateway:cli", chat_id: str = "u1") -> OutboundEvent:
    event = OutboundEvent.text_reply(channel=channel, chat_id=chat_id, text="answer")
    event.is_final = True
    event.message_kind = "final"
    return event


def _tool_delivery(text: str, *, correlation_id: str, part: int) -> OutboundEvent:
    return OutboundEvent.text_reply(
        channel="gateway:cli",
        chat_id="u1",
        text=text,
        metadata={
            "_inbound_event_id": correlation_id,
            "_tool_delivery": True,
            "_artifact_delivery_id": "delivery-1",
            "_artifact_part": part,
            "_artifact_parts": 2,
        },
    )


def _live_socket() -> MagicMock:
    ws = MagicMock()
    ws.closed = False
    ws.send_json = AsyncMock()
    return ws


@pytest.mark.asyncio
async def test_cancelled_pre_enqueue_publish_discards_interrupt_admission():
    gw = _gateway()
    interrupts = InterruptManager()
    gw._agent_loop.interrupt = interrupts
    entered = asyncio.Event()
    release = asyncio.Event()

    async def reject_after_wait(_event):
        entered.set()
        await release.wait()
        return False

    gw._bus.publish_inbound = reject_after_wait
    event = InboundEvent.text_message(
        channel="gateway:cli",
        sender_id="u1",
        chat_id="u1",
        text="work",
    )
    interrupts.admit(event.session_key, event.event_id)

    publish = asyncio.create_task(gw._publish_accepted_turn(event))
    await entered.wait()
    publish.cancel()
    release.set()
    with pytest.raises(asyncio.CancelledError):
        await publish

    assert not interrupts._admitted
    assert interrupts.interrupt(event.session_key) is False


@pytest.mark.asyncio
async def test_pre_enqueue_publish_exception_discards_interrupt_admission():
    gw = _gateway()
    interrupts = InterruptManager()
    gw._agent_loop.interrupt = interrupts
    gw._bus.publish_inbound = AsyncMock(side_effect=RuntimeError("queue unavailable"))
    event = InboundEvent.text_message(
        channel="gateway:cli",
        sender_id="u1",
        chat_id="u1",
        text="work",
    )
    interrupts.admit(event.session_key, event.event_id)

    with pytest.raises(RuntimeError, match="queue unavailable"):
        await gw._publish_accepted_turn(event)

    assert not interrupts._admitted


# ── gateway outbound receipts ────────────────────────────────────────────────


@pytest.mark.asyncio
async def test_final_with_no_client_reports_failure():
    """The core defect: nobody received the answer, yet it was reported OK."""
    gw = _gateway()

    result = await gw._handle_outbound(_final())

    assert result is not None, "must not stay silent — silence reads as ACCEPTED"
    assert result.success is False
    assert "no live gateway client" in result.error


@pytest.mark.asyncio
async def test_final_to_live_socket_reports_success():
    gw = _gateway()
    gw._ws_clients["gateway:cli:u1"] = _live_socket()

    result = await gw._handle_outbound(_final())

    assert result is not None and result.success is True


@pytest.mark.asyncio
async def test_closed_socket_reports_failure():
    """A socket object that exists but is closed is not a delivery target."""
    gw = _gateway()
    ws = _live_socket()
    ws.closed = True
    gw._ws_clients["gateway:cli:u1"] = ws

    result = await gw._handle_outbound(_final())

    assert result is not None and result.success is False


@pytest.mark.asyncio
async def test_http_waiter_alone_counts_as_delivered():
    """A blocked HTTP caller will receive this reply, so it is delivered even
    with no WebSocket attached."""
    gw = _gateway()
    future: asyncio.Future = asyncio.get_running_loop().create_future()
    gw._pending_http["evt-1"] = future
    event = _final()
    event.metadata["_inbound_event_id"] = "evt-1"

    result = await gw._handle_outbound(event)

    assert result is not None and result.success is True
    assert future.done()


@pytest.mark.asyncio
async def test_http_waiter_collects_tool_deliveries_until_real_terminal():
    """Multipart tool output must not consume the turn's one HTTP waiter."""
    gw = _gateway()
    future: asyncio.Future = asyncio.get_running_loop().create_future()
    gw._pending_http["evt-multipart"] = future
    gw._message_idempotency.context_for_event = AsyncMock(
        return_value={
            "transport": "http",
            "wait": True,
            "session_key": "gateway:cli:u1",
        }
    )
    gw._message_idempotency.complete_event = AsyncMock(return_value=True)

    first = _tool_delivery("part one", correlation_id="evt-multipart", part=1)
    second = _tool_delivery("part two", correlation_id="evt-multipart", part=2)
    first_result = await gw._handle_outbound(first)
    assert first_result.success is True and first_result.deferred is True
    # Snapshot semantics: mutation after an accepted delivery cannot rewrite
    # the eventual synchronous response.
    first.metadata["_artifact_part"] = 99
    duplicate_result = await gw._handle_outbound(
        _tool_delivery("part one", correlation_id="evt-multipart", part=1)
    )
    assert duplicate_result.success is True and duplicate_result.deferred is True
    second_result = await gw._handle_outbound(second)
    assert second_result.success is True and second_result.deferred is True

    assert not future.done()
    assert "evt-multipart" in gw._pending_http
    gw._message_idempotency.context_for_event.assert_not_awaited()
    gw._message_idempotency.complete_event.assert_not_awaited()

    terminal = _final()
    terminal.metadata["_inbound_event_id"] = "evt-multipart"
    result = await gw._handle_outbound(terminal)

    assert result is not None and result.success is True
    assert result.deferred is False
    payload = future.result()
    assert [frame["text"] for frame in payload["tool_deliveries"]] == [
        "part one",
        "part two",
    ]
    assert payload["tool_deliveries"][0]["metadata"]["_artifact_part"] == 1
    assert payload["text"] == "answer"
    assert "evt-multipart" not in gw._pending_http
    assert gw._pending_http_tool_deliveries == {}
    assert gw._pending_http_tool_delivery_frames == 0
    assert gw._pending_http_tool_delivery_chars == 0
    gw._message_idempotency.context_for_event.assert_awaited_once_with(
        "evt-multipart"
    )
    gw._message_idempotency.complete_event.assert_awaited_once()
    completed_payload = gw._message_idempotency.complete_event.await_args.kwargs[
        "payload"
    ]
    assert [
        frame["text"]
        for frame in completed_payload["reply"]["tool_deliveries"]
    ] == ["part one", "part two"]


@pytest.mark.asyncio
async def test_http_tool_delivery_buffer_overflow_reports_failure_even_with_ws():
    gw = _gateway()
    gw._MAX_HTTP_TOOL_DELIVERY_FRAMES = 1
    gw._ws_clients["gateway:cli:u1"] = _live_socket()
    future: asyncio.Future = asyncio.get_running_loop().create_future()
    gw._pending_http["evt-overflow"] = future

    first = await gw._handle_outbound(
        _tool_delivery("part one", correlation_id="evt-overflow", part=1)
    )
    overflow = await gw._handle_outbound(
        _tool_delivery("part two", correlation_id="evt-overflow", part=2)
    )

    assert first is not None and first.success is True
    assert first.deferred is False  # the live WebSocket received this part
    assert overflow is not None and overflow.success is False
    assert "buffer limit exceeded" in overflow.error
    assert not future.done()
    frames, _char_count, _updated_at = gw._pending_http_tool_deliveries[
        "evt-overflow"
    ]
    assert [frame["text"] for frame in frames] == ["part one"]
    gw._clear_http_tool_deliveries("evt-overflow")
    future.cancel()


@pytest.mark.asyncio
async def test_gateway_stop_cancels_waiter_and_clears_tool_delivery_buffers():
    gw = _gateway()
    future: asyncio.Future = asyncio.get_running_loop().create_future()
    gw._pending_http["evt-stop"] = future
    result = await gw._handle_outbound(
        _tool_delivery("part one", correlation_id="evt-stop", part=1)
    )
    assert result is not None and result.success is True
    assert result.deferred is True
    assert gw._pending_http_tool_deliveries

    await gw.stop()

    assert future.cancelled()
    assert gw._pending_http == {}
    assert gw._pending_http_tool_deliveries == {}
    assert gw._pending_http_tool_delivery_frames == 0
    assert gw._pending_http_tool_delivery_chars == 0


@pytest.mark.asyncio
async def test_bus_receipt_preserves_deferred_http_tool_delivery():
    gw = _gateway()
    future: asyncio.Future = asyncio.get_running_loop().create_future()
    gw._pending_http["evt-deferred"] = future

    receipt = await gw._bus.publish_outbound(
        _tool_delivery("part one", correlation_id="evt-deferred", part=1)
    )

    assert receipt.ok is True
    assert receipt.stage is DeliveryStage.DELIVERED
    assert receipt.detail["deferred"] is True
    assert not future.done()
    gw._clear_http_tool_deliveries("evt-deferred")
    future.cancel()


@pytest.mark.asyncio
async def test_buffer_capacity_evicts_oldest_timed_out_waiter():
    gw = _gateway()
    gw._MAX_HTTP_TOOL_DELIVERY_TOTAL_FRAMES = 1
    old_future: asyncio.Future = asyncio.get_running_loop().create_future()
    gw._pending_http["evt-old"] = old_future
    old = await gw._handle_outbound(
        _tool_delivery("old part", correlation_id="evt-old", part=1)
    )
    assert old is not None and old.deferred is True

    # Simulate the HTTP request timing out while the admitted turn continues.
    gw._pending_http.pop("evt-old")
    old_future.cancel()
    new_future: asyncio.Future = asyncio.get_running_loop().create_future()
    gw._pending_http["evt-new"] = new_future
    new = await gw._handle_outbound(
        _tool_delivery("new part", correlation_id="evt-new", part=1)
    )

    assert new is not None and new.success is True and new.deferred is True
    assert "evt-old" not in gw._pending_http_tool_deliveries
    assert "evt-new" in gw._pending_http_tool_deliveries
    gw._clear_http_tool_deliveries("evt-new")
    new_future.cancel()


@pytest.mark.asyncio
async def test_orphaned_tool_delivery_buffer_expires_at_ttl():
    gw = _gateway()
    future: asyncio.Future = asyncio.get_running_loop().create_future()
    gw._pending_http["evt-expired"] = future
    result = await gw._handle_outbound(
        _tool_delivery("old part", correlation_id="evt-expired", part=1)
    )
    assert result is not None and result.deferred is True

    gw._pending_http.pop("evt-expired")
    future.cancel()
    gw._HTTP_TOOL_DELIVERY_TTL_SECONDS = -1
    gw._purge_http_tool_deliveries()

    assert gw._pending_http_tool_deliveries == {}
    assert gw._pending_http_tool_delivery_frames == 0
    assert gw._pending_http_tool_delivery_chars == 0


@pytest.mark.asyncio
async def test_non_gateway_event_gets_no_opinion():
    """This is a GLOBAL outbound handler: it sees Telegram/weixin/etc. events
    too. Returning a receipt for them would fault deliveries that succeeded on
    their own channel."""
    gw = _gateway()

    assert await gw._handle_outbound(_final(channel="telegram", chat_id="c")) is None


@pytest.mark.asyncio
async def test_dropped_event_gets_no_opinion():
    """``_drop`` means another layer already owns this event's fate."""
    gw = _gateway()
    event = _final()
    event.metadata["_drop"] = True

    assert await gw._handle_outbound(event) is None


@pytest.mark.asyncio
async def test_interim_stream_frame_does_not_fault_the_turn():
    """Interim frames are level-triggered progress; the final carries the full
    text. A dropped one must not mark the turn failed."""
    gw = _gateway()
    interim = OutboundEvent.text_reply(channel="gateway:cli", chat_id="u1", text="partial")
    interim.is_final = False
    interim.message_kind = "stream"

    assert await gw._handle_outbound(interim) is None


@pytest.mark.asyncio
async def test_bus_aggregates_gateway_failure_as_not_ok():
    """End to end through the bus: the receipt must actually reach the caller's
    ``DeliveryResult.ok``, which is what loop.py keys the cron/task outcome on."""
    gw = _gateway()
    bus = MessageBus()
    bus.subscribe_outbound_global(gw._handle_outbound)

    receipt = await bus.publish_outbound(_final())

    assert receipt.ok is False
    assert receipt.stage is DeliveryStage.FAILED


@pytest.mark.asyncio
async def test_bus_aggregates_live_delivery_as_ok():
    gw = _gateway()
    gw._ws_clients["gateway:cli:u1"] = _live_socket()
    bus = MessageBus()
    bus.subscribe_outbound_global(gw._handle_outbound)

    receipt = await bus.publish_outbound(_final())

    assert receipt.ok is True


# ── send_file honesty ────────────────────────────────────────────────────────


class _Channel:
    def __init__(self, supports_files: bool):
        self.supports_files = supports_files


def _tool(tmp_path, receipt, channel):
    from echo_agent.agent.tools.send_file import SendFileTool

    async def _publish(_event):
        return receipt

    return SendFileTool(
        str(tmp_path),
        publish_fn=_publish,
        channel_lookup=(lambda _n: channel) if channel is not None else None,
    )


@pytest.fixture
def a_file(tmp_path):
    path = tmp_path / "report.docx"
    path.write_bytes(b"x" * 32)
    return path


@pytest.mark.asyncio
async def test_send_file_refuses_text_only_channel(tmp_path, a_file):
    """Telegram/Slack/email send the caption and drop the attachment. Saying so
    lets the model find another route instead of believing the file arrived."""
    tool = _tool(tmp_path, DeliveryResult(DeliveryStage.ACCEPTED, "telegram"), _Channel(False))

    result = await tool.execute(
        {"channel": "telegram", "chat_id": "c", "file_path": str(a_file)}, None,
    )

    assert result.success is False
    assert "cannot send files" in result.error


@pytest.mark.asyncio
async def test_send_file_succeeds_on_capable_channel(tmp_path, a_file):
    tool = _tool(tmp_path, DeliveryResult(DeliveryStage.DELIVERED, "weixin"), _Channel(True))

    result = await tool.execute(
        {"channel": "weixin", "chat_id": "c", "file_path": str(a_file)}, None,
    )

    assert result.success is True
    assert "report.docx" in result.output


@pytest.mark.asyncio
async def test_send_file_reports_no_handler(tmp_path, a_file):
    tool = _tool(tmp_path, DeliveryResult(DeliveryStage.NO_HANDLER, "weixin"), _Channel(True))

    result = await tool.execute(
        {"channel": "weixin", "chat_id": "c", "file_path": str(a_file)}, None,
    )

    assert result.success is False
    assert "not delivered" in result.error


@pytest.mark.asyncio
async def test_send_file_reports_channel_refusal(tmp_path, a_file):
    tool = _tool(
        tmp_path,
        DeliveryResult(DeliveryStage.FAILED, "weixin", error="upload 413"),
        _Channel(True),
    )

    result = await tool.execute(
        {"channel": "weixin", "chat_id": "c", "file_path": str(a_file)}, None,
    )

    assert result.success is False
    assert "upload 413" in result.error


@pytest.mark.asyncio
async def test_send_file_tolerates_receiptless_publisher(tmp_path, a_file):
    """Callers whose publish_fn returns None (older wiring, and much of the test
    suite) must keep working rather than being read as failure."""
    tool = _tool(tmp_path, None, _Channel(True))

    result = await tool.execute(
        {"channel": "weixin", "chat_id": "c", "file_path": str(a_file)}, None,
    )

    assert result.success is True


@pytest.mark.asyncio
async def test_send_file_without_lookup_does_not_block(tmp_path, a_file):
    """No channel_lookup wired: fall back to trusting the receipt rather than
    refusing every channel."""
    tool = _tool(tmp_path, DeliveryResult(DeliveryStage.DELIVERED, "x"), None)

    result = await tool.execute(
        {"channel": "x", "chat_id": "c", "file_path": str(a_file)}, None,
    )

    assert result.success is True


@pytest.mark.asyncio
async def test_send_file_refuses_unknown_adapter_when_lookup_is_wired(tmp_path, a_file):
    """Gateway pseudo-channels have no uploader and must fail before publish."""
    from echo_agent.agent.tools.send_file import SendFileTool

    publish = AsyncMock(return_value=DeliveryResult(
        DeliveryStage.DELIVERED, "gateway:cli",
    ))
    tool = SendFileTool(
        str(tmp_path),
        publish_fn=publish,
        channel_lookup=lambda _name: None,
    )

    result = await tool.execute(
        {
            "channel": "gateway:cli",
            "chat_id": "c",
            "file_path": str(a_file),
        },
        None,
    )

    assert result.success is False
    assert result.error_kind == "business"
    assert "no file-capable adapter" in result.error
    publish.assert_not_awaited()


def test_capable_channels_declare_support():
    """The flag must track reality: qqbot and weixin are the two adapters that
    consume structured FILE/IMAGE blocks, and text-only ones must not claim it."""
    from echo_agent.channels.base import BaseChannel
    from echo_agent.channels.telegram import TelegramChannel
    from echo_agent.channels.weixin import WeixinChannel

    assert BaseChannel.supports_files is False, "default must be conservative"
    assert WeixinChannel.supports_files is True
    assert getattr(TelegramChannel, "supports_files", False) is False
