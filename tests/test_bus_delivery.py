import pytest

from echo_agent.bus.delivery import DeliveryStage, DeliveryResult
from echo_agent.bus.events import OutboundEvent
from echo_agent.bus.queue import MessageBus
from echo_agent.channels.base import SendResult


def _evt(channel="cli"):
    return OutboundEvent(channel=channel, chat_id="c1", content=[])


def test_ok_true_for_delivered_and_accepted():
    assert DeliveryResult(DeliveryStage.DELIVERED, "cli").ok is True
    assert DeliveryResult(DeliveryStage.ACCEPTED, "cli").ok is True


def test_ok_false_for_failed_and_no_handler():
    assert DeliveryResult(DeliveryStage.FAILED, "cli", error="x").ok is False
    assert DeliveryResult(DeliveryStage.NO_HANDLER, "cli").ok is False


def test_from_send_result_maps_success_and_failure():
    ok = DeliveryResult.from_send_result(SendResult(success=True, message_id="m1"), "cli")
    assert ok.stage is DeliveryStage.DELIVERED
    bad = DeliveryResult.from_send_result(SendResult(success=False, error="boom"), "cli")
    assert bad.stage is DeliveryStage.FAILED
    assert bad.error == "boom"


@pytest.mark.asyncio
async def test_publish_no_handler_returns_no_handler():
    bus = MessageBus()
    res = await bus.publish_outbound(_evt())
    assert res.stage is DeliveryStage.NO_HANDLER


@pytest.mark.asyncio
async def test_publish_handler_returning_none_is_accepted():
    bus = MessageBus()
    async def h(e): return None
    bus.subscribe_outbound("cli", h)
    res = await bus.publish_outbound(_evt())
    assert res.stage is DeliveryStage.ACCEPTED


@pytest.mark.asyncio
async def test_publish_send_result_success_is_delivered():
    bus = MessageBus()
    async def h(e): return SendResult(success=True, message_id="m")
    bus.subscribe_outbound("cli", h)
    res = await bus.publish_outbound(_evt())
    assert res.stage is DeliveryStage.DELIVERED


@pytest.mark.asyncio
async def test_publish_send_result_failure_is_failed():
    bus = MessageBus()
    async def h(e): return SendResult(success=False, error="down")
    bus.subscribe_outbound("cli", h)
    res = await bus.publish_outbound(_evt())
    assert res.stage is DeliveryStage.FAILED
    assert res.error == "down"


@pytest.mark.asyncio
async def test_publish_handler_exception_is_failed():
    bus = MessageBus()
    async def h(e): raise RuntimeError("boom")
    bus.subscribe_outbound("cli", h)
    res = await bus.publish_outbound(_evt())
    assert res.stage is DeliveryStage.FAILED
