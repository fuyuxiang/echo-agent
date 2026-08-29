from __future__ import annotations

import asyncio
from types import SimpleNamespace

import pytest

from echo_agent.app import AppRuntime
from echo_agent.bus.events import InboundEvent, OutboundEvent
from echo_agent.bus.queue import MessageBus
from echo_agent.channels.base import BaseChannel, SendResult
from echo_agent.channels.manager import ChannelManager
from echo_agent.config.schema import ChannelsConfig


@pytest.mark.asyncio
async def test_runtime_drains_bus_before_stopping_output_targets_and_agent():
    order: list[str] = []

    def component(name: str, method: str):
        async def stop() -> None:
            order.append(name)

        return SimpleNamespace(**{method: stop})

    ctx = SimpleNamespace(
        health=component("health", "stop"),
        scheduler=None,
        task_dispatcher=None,
        channels=component("channels", "stop_all"),
        bus=component("bus", "stop"),
        agent=component("agent", "stop"),
        storage=component("storage", "close"),
    )
    runtime = AppRuntime(ctx)
    runtime._started = True
    runtime._gateway = component("gateway", "stop")

    await runtime.stop()

    assert order == ["health", "bus", "gateway", "channels", "agent", "storage"]


@pytest.mark.asyncio
async def test_partial_start_rollback_uses_the_same_drain_first_teardown():
    order: list[str] = []

    def lifecycle(name: str):
        async def start() -> None:
            order.append(f"{name}-start")

        async def stop() -> None:
            order.append(f"{name}-stop")

        return SimpleNamespace(start=start, stop=stop)

    bus = lifecycle("bus")
    agent = lifecycle("agent")

    async def start_channels() -> None:
        order.append("channels-start")
        raise RuntimeError("transport bind failed")

    async def stop_channels() -> None:
        order.append("channels-stop")

    async def close_storage() -> None:
        order.append("storage-close")

    async def stop_health() -> None:
        order.append("health-stop")

    ctx = SimpleNamespace(
        instance_lock=None,
        bus=bus,
        agent=agent,
        channels=SimpleNamespace(
            start_all=start_channels,
            stop_all=stop_channels,
            active_channels=[],
        ),
        health=SimpleNamespace(stop=stop_health),
        scheduler=None,
        task_dispatcher=None,
        storage=SimpleNamespace(close=close_storage),
    )
    runtime = AppRuntime(ctx)

    try:
        with pytest.raises(RuntimeError, match="transport bind failed"):
            await runtime.start()
    finally:
        # This is the same finally boundary used by run()/run_gateway().
        await runtime.stop()

    assert order == [
        "bus-start",
        "agent-start",
        "channels-start",
        "health-stop",
        "bus-stop",
        "channels-stop",
        "agent-stop",
        "storage-close",
    ]


class _DrainChannel(BaseChannel):
    name = "test"

    def __init__(self, bus: MessageBus, delivered: list[str]) -> None:
        super().__init__(SimpleNamespace(reactions_enabled=False), bus)
        self._running = True
        self._delivered = delivered

    async def start(self) -> None:
        self._running = True

    async def stop(self) -> None:
        self._running = False

    async def send(self, event: OutboundEvent) -> SendResult:
        # This assertion is the transport half of the shutdown contract: bus
        # drain may only dispatch while the channel is still usable.
        assert self._running
        self._delivered.append(event.text)
        return SendResult(success=True, message_id="channel-final")


class _DrainGateway:
    def __init__(self, bus: MessageBus, event_id: str) -> None:
        self._bus = bus
        self._running = True
        self.reply = asyncio.get_running_loop().create_future()
        self._event_id = event_id
        bus.subscribe_outbound_global(self._handle_outbound)

    async def _handle_outbound(self, event: OutboundEvent) -> SendResult | None:
        if event.channel != "gateway:web":
            return None
        assert self._running
        if event.metadata.get("_inbound_event_id") == self._event_id:
            self.reply.set_result(event.text)
            return SendResult(success=True, message_id="gateway-final")
        return None

    async def stop(self) -> None:
        self._running = False
        self._bus.unsubscribe_outbound_global(self._handle_outbound)
        if not self.reply.done():
            self.reply.cancel()


class _DrainAgent:
    def __init__(self, bus: MessageBus, blocker: asyncio.Event) -> None:
        self._bus = bus
        self._blocker = blocker
        self.blocked = asyncio.Event()
        self._running = True
        bus.subscribe_inbound(self._on_inbound)

    async def _on_inbound(self, event: InboundEvent) -> None:
        assert self._running
        if event.text == "block":
            self.blocked.set()
            await self._blocker.wait()
            return
        reply = OutboundEvent.text_reply(event.channel, event.chat_id, f"done:{event.text}")
        reply.metadata["_inbound_event_id"] = event.event_id
        await self._bus.publish_outbound(reply)

    async def stop(self) -> None:
        self._running = False
        self._bus.unsubscribe_inbound(self._on_inbound)


@pytest.mark.asyncio
async def test_runtime_bus_drain_delivers_queued_channel_and_gateway_finals():
    """Accepted work keeps both outbound target types until drain completes.

    A saturated single concurrency slot leaves the two real replies pending in
    MessageBus.  The shutdown task must first make the bus non-accepting, while
    the ChannelManager transport and Gateway waiter are both still live, then
    let the queued turns finish before tearing either target down.
    """
    bus = MessageBus(max_concurrency=1)
    delivered: list[str] = []
    channels = ChannelManager(ChannelsConfig(), bus)
    channel = _DrainChannel(bus, delivered)
    channels._channels[channel.name] = channel

    release = asyncio.Event()
    agent = _DrainAgent(bus, release)
    gateway_event = InboundEvent.text_message(
        "gateway:web", "user", "gateway-chat", "gateway",
    )
    gateway = _DrainGateway(bus, gateway_event.event_id)

    await bus.start()
    assert await bus.publish_inbound(
        InboundEvent.text_message("test", "user", "channel-chat", "block")
    )
    await agent.blocked.wait()
    assert await bus.publish_inbound(
        InboundEvent.text_message("test", "user", "channel-chat", "channel")
    )
    assert await bus.publish_inbound(gateway_event)

    async def no_op() -> None:
        return None

    ctx = SimpleNamespace(
        health=SimpleNamespace(stop=no_op),
        scheduler=None,
        task_dispatcher=None,
        channels=channels,
        bus=bus,
        agent=agent,
        storage=SimpleNamespace(close=no_op),
    )
    runtime = AppRuntime(ctx)
    runtime._started = True
    runtime._gateway = gateway

    stop_task = asyncio.create_task(runtime.stop())
    while bus._accepting:
        await asyncio.sleep(0)

    # Regression assertion: the old order had already cancelled the HTTP
    # waiter and stopped/cleared the channel by the time bus.stop began.
    assert gateway._running
    assert channel.is_running
    release.set()
    await stop_task

    assert delivered == ["done:channel"]
    assert await gateway.reply == "done:gateway"
    assert not channel.is_running
    assert not gateway._running
