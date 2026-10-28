"""Protocol failures must release ownership and preserve business outcome."""
import asyncio
from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest

from echo_agent.a2a.models import AgentCard, A2AMessage, A2ATask, TaskState
from echo_agent.a2a.server import A2AServer
from echo_agent.mcp.client import MCPClient


@pytest.mark.asyncio
async def test_failed_mcp_handshake_closes_owned_transport():
    transport = SimpleNamespace(is_connected=True, connect=AsyncMock(), close=AsyncMock(),
                                receive=AsyncMock(side_effect=ConnectionError('EOF')), send=AsyncMock())
    client = MCPClient('bad-handshake', transport)
    with pytest.raises(ConnectionError):
        await client.connect(timeout=0.1)
    try:
        transport.close.assert_awaited_once()
        assert not client.is_connected
        assert client._reader_task is None
    finally:
        await client.disconnect()


@pytest.mark.asyncio
async def test_cancelled_mcp_call_notifies_server_and_clears_pending():
    transport = SimpleNamespace(is_connected=True, send=AsyncMock(), close=AsyncMock())
    client = MCPClient('slow', transport)
    client._reader_alive = True
    call = asyncio.create_task(client.call_tool('slow', timeout=30))
    await asyncio.sleep(0)
    call.cancel()
    with pytest.raises(asyncio.CancelledError):
        await call
    assert not client._pending
    assert any(c.args[0].get('method') == 'notifications/cancelled' for c in transport.send.call_args_list)


@pytest.mark.asyncio
async def test_a2a_uses_structured_business_outcome():
    class Runtime:
        async def process_direct(self, *args, **kwargs):
            return 'Error: provider unavailable'

        async def process_direct_result(self, *args, **kwargs):
            return SimpleNamespace(response_text='Error: provider unavailable',
                                   task_incomplete=True, termination_reason='model_error')
    server = A2AServer(Runtime(), AgentCard())
    task = await server._process_task(A2ATask(messages=[A2AMessage.text('user', 'hi')]))
    assert task.state == TaskState.FAILED
    assert task.metadata['termination_reason'] == 'model_error'
