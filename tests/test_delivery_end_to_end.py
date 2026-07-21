"""End-to-end delivery receipt tests for ChannelManager.

Verifies that a channel's SendResult is surfaced through the global outbound
handler (_filter_and_dispatch) and aggregated by the bus into a real
DeliveryStage, rather than being silently dropped.
"""

import pytest

from echo_agent.bus.queue import MessageBus
from echo_agent.bus.events import OutboundEvent, ContentBlock, ContentType
from echo_agent.bus.delivery import DeliveryStage
from echo_agent.channels.manager import ChannelManager
from echo_agent.channels.base import BaseChannel, SendResult


class _StubConfig:
    """Minimal ChannelsConfig stand-in: only fields the manager reads."""

    send_progress = False
    send_tool_hints = False


class _FakeChannel(BaseChannel):
    name = "fake"

    def __init__(self, send_ok: bool):
        self._send_ok = send_ok
        self._running = True
        self.config = type("C", (), {"reactions_enabled": False})()

    async def start(self): ...

    async def stop(self): ...

    @property
    def is_running(self):
        return True

    async def send_typing(self, *a, **k): ...

    async def stop_typing(self, *a, **k): ...

    async def send(self, event: OutboundEvent) -> SendResult:
        return SendResult(
            success=self._send_ok,
            message_id="m1",
            error="" if self._send_ok else "platform down",
        )


def _final_event(channel="fake"):
    e = OutboundEvent(
        channel=channel,
        chat_id="c1",
        content=[ContentBlock(type=ContentType.TEXT, text="hi")],
    )
    e.is_final = True
    e.message_kind = "final"
    return e


@pytest.mark.asyncio
async def test_failed_send_surfaces_as_failed_delivery():
    bus = MessageBus()
    mgr = ChannelManager(_StubConfig(), bus)
    mgr._channels["fake"] = _FakeChannel(send_ok=False)
    res = await bus.publish_outbound(_final_event())
    assert res.stage is DeliveryStage.FAILED
    assert "platform down" in (res.error or "")


@pytest.mark.asyncio
async def test_ok_send_surfaces_as_delivered():
    bus = MessageBus()
    mgr = ChannelManager(_StubConfig(), bus)
    mgr._channels["fake"] = _FakeChannel(send_ok=True)
    res = await bus.publish_outbound(_final_event())
    assert res.stage is DeliveryStage.DELIVERED
