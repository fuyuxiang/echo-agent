"""Tool-list notifications must not block the reader needed for their RPC."""

import asyncio

import pytest

from echo_agent.agent.tools.registry import ToolRegistry
from echo_agent.config.schema import MCPServerConfig
from echo_agent.mcp.manager import MCPManager


class QueueServerTransport:
    def __init__(self):
        self.is_connected = False
        self.inbound = asyncio.Queue()
        self.tools = [{"name": "first", "description": "read a value"}]
        self.list_calls = 0
        self.hold_listing = None

    async def connect(self, timeout=60):
        self.is_connected = True

    async def close(self):
        self.is_connected = False

    async def send(self, message):
        method = message.get("method")
        if method == "initialize":
            result = {"protocolVersion": "2025-06-18", "capabilities": {"tools": {"listChanged": True}}}
        elif method == "tools/list":
            self.list_calls += 1
            tools = self.tools
            if self.hold_listing is not None:
                await self.hold_listing.wait()
            result = {"tools": tools}
        else:
            return
        await self.inbound.put({"jsonrpc": "2.0", "id": message["id"], "result": result})

    async def receive(self):
        return await self.inbound.get()


async def connected_manager(tmp_path, monkeypatch):
    transport = QueueServerTransport()
    manager = MCPManager(tmp_path)

    async def create(*args):
        return transport

    monkeypatch.setattr(manager, "_create_transport", create)
    await manager.start_all({"srv": MCPServerConfig(command="test-server", connect_timeout=1)})
    registry = ToolRegistry()
    await manager.discover_tools(registry)
    return manager, transport, registry


@pytest.mark.asyncio
async def test_tool_list_notification_refreshes_through_actual_reader(tmp_path, monkeypatch):
    manager, transport, registry = await connected_manager(tmp_path, monkeypatch)
    try:
        assert "mcp_srv_first" in registry.tool_names
        transport.tools = [{"name": "second", "description": "read another value"}]
        await transport.inbound.put({"jsonrpc": "2.0", "method": "notifications/tools/list_changed", "params": {}})

        async def refreshed():
            while "mcp_srv_second" not in registry.tool_names:
                await asyncio.sleep(0.001)

        await asyncio.wait_for(refreshed(), timeout=0.3)
        assert "mcp_srv_first" not in registry.tool_names
        assert manager.get_client("srv")._reader_alive
    finally:
        await manager.stop_all()


@pytest.mark.asyncio
async def test_notification_burst_is_coalesced_and_stop_reaps_refresh(tmp_path, monkeypatch):
    manager, transport, registry = await connected_manager(tmp_path, monkeypatch)
    transport.hold_listing = asyncio.Event()
    for _ in range(50):
        await transport.inbound.put({"jsonrpc": "2.0", "method": "notifications/tools/list_changed", "params": {}})
    await asyncio.sleep(0.03)
    try:
        assert transport.list_calls == 2  # Initial discovery + one in-flight refresh.
        assert len(manager._refresh_tasks) == 1
    finally:
        await asyncio.wait_for(manager.stop_all(), timeout=0.5)
    assert manager._refresh_tasks == {}
    assert not transport.is_connected


@pytest.mark.asyncio
async def test_change_during_inflight_refresh_is_not_lost(tmp_path, monkeypatch):
    manager, transport, registry = await connected_manager(tmp_path, monkeypatch)
    try:
        transport.hold_listing = asyncio.Event()
        transport.tools = [{"name": "middle"}]
        await transport.inbound.put({"method": "notifications/tools/list_changed"})
        while transport.list_calls != 2:
            await asyncio.sleep(0.001)
        transport.tools = [{"name": "latest"}]
        await transport.inbound.put({"method": "notifications/tools/list_changed"})
        await asyncio.sleep(0.01)
        transport.hold_listing.set()

        async def latest():
            while "mcp_srv_latest" not in registry.tool_names:
                await asyncio.sleep(0.001)

        await asyncio.wait_for(latest(), timeout=0.3)
        assert "mcp_srv_first" not in registry.tool_names
        assert "mcp_srv_middle" not in registry.tool_names
    finally:
        await manager.stop_all()
