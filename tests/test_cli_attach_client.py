import pytest

from echo_agent.cli.attach_client import build_ws_url, NoGatewayError, connect_ws


def test_build_ws_url_loopback():
    assert build_ws_url("127.0.0.1", 9000, "/ws") == "ws://127.0.0.1:9000/ws"


def test_build_ws_url_normalizes_missing_leading_slash():
    assert build_ws_url("127.0.0.1", 9000, "ws") == "ws://127.0.0.1:9000/ws"


@pytest.mark.asyncio
async def test_connect_ws_raises_no_gateway_on_refused():
    import aiohttp
    async with aiohttp.ClientSession() as s:
        # 9 是 discard 端口，本机通常无监听 → 连接被拒
        with pytest.raises(NoGatewayError) as ei:
            await connect_ws(s, "ws://127.0.0.1:9/ws")
    assert "echo-agent gateway" in str(ei.value)
