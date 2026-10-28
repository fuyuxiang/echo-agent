"""A Dashboard channel restart must detach the previous adapter's sender."""

import pytest

from echo_agent.bus.queue import MessageBus
from echo_agent.channels.manager import ChannelManager
from echo_agent.config.schema import ChannelsConfig


@pytest.mark.asyncio
async def test_channel_restarts_keep_one_outbound_sender():
    config = ChannelsConfig()
    config.cron.enabled = True
    bus = MessageBus()
    manager = ChannelManager(config, bus)
    try:
        first = await manager.start_channel("cron")
        for _ in range(3):
            await manager.restart_channel("cron")
        assert len(bus._outbound_handlers["cron"]) == 1
        assert first.send not in bus._outbound_handlers["cron"]
    finally:
        await manager.stop_all()
    assert not bus._outbound_handlers.get("cron")
