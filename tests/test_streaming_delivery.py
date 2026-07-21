"""TokenStreamPublisher.finalize returns a real DeliveryResult receipt.

Regression guard for the "streamed delivery failed but upstream still thought it
succeeded, so it never republished the plain final message" class of bug. The
publisher must surface publish_outbound's DeliveryResult instead of a bare bool.
"""

import pytest

from echo_agent.agent.streaming import TokenStreamPublisher
from echo_agent.bus.delivery import DeliveryResult, DeliveryStage
from echo_agent.bus.events import InboundEvent


class _Bus:
    def __init__(self, result):
        self._r = result
        self.calls = []

    async def publish_outbound(self, event):
        self.calls.append(event)
        return self._r


@pytest.mark.asyncio
async def test_finalize_returns_delivery_result():
    bus = _Bus(DeliveryResult(DeliveryStage.DELIVERED, "cli"))
    ev = InboundEvent(channel="cli", chat_id="c1")
    pub = TokenStreamPublisher(bus, ev, enabled=True, flush_chars=120, flush_interval_ms=1200)
    res = await pub.finalize("final answer")
    assert res.stage is DeliveryStage.DELIVERED


@pytest.mark.asyncio
async def test_finalize_disabled_returns_no_handler():
    ev = InboundEvent(channel="cli", chat_id="c1")
    pub = TokenStreamPublisher(_Bus(None), ev, enabled=False, flush_chars=120, flush_interval_ms=1200)
    res = await pub.finalize("x")
    assert res.stage is DeliveryStage.NO_HANDLER


@pytest.mark.asyncio
async def test_finalize_failed_receipt_is_surfaced():
    # A failed platform send must be reported so callers can republish.
    bus = _Bus(DeliveryResult(DeliveryStage.FAILED, "cli", error="boom"))
    ev = InboundEvent(channel="cli", chat_id="c1")
    pub = TokenStreamPublisher(bus, ev, enabled=True, flush_chars=120, flush_interval_ms=1200)
    res = await pub.finalize("final answer")
    assert res.stage is DeliveryStage.FAILED
    assert res.ok is False


@pytest.mark.asyncio
async def test_finalize_after_nonfinal_returns_final_receipt():
    # When a non-final chunk was already streamed, finalize republishes the full
    # text and must return that final publish's receipt.
    bus = _Bus(DeliveryResult(DeliveryStage.DELIVERED, "cli"))
    ev = InboundEvent(channel="cli", chat_id="c1")
    pub = TokenStreamPublisher(bus, ev, enabled=True, flush_chars=120, flush_interval_ms=1200)
    pub._sent_nonfinal = True
    pub._full_text = "partial"
    pub._pending = ""
    res = await pub.finalize("partial complete")
    assert res.stage is DeliveryStage.DELIVERED
    assert bus.calls[-1].is_final is True
