import aiohttp
import pytest

from echo_agent.gateway.ws_session import resolve_client_session_key


def test_empty_request_falls_back_to_gateway_key():
    key, err = resolve_client_session_key(None, platform="wechat", chat_id="u1")
    assert err == ""
    assert key == "gateway:wechat:u1"


def test_cli_prefix_accepted_verbatim():
    key, err = resolve_client_session_key("cli:alice", platform="cli", chat_id="alice")
    assert err == ""
    assert key == "cli:alice"


def test_non_whitelisted_prefix_rejected():
    key, err = resolve_client_session_key(
        "gateway:wechat:victim", platform="cli", chat_id="alice"
    )
    assert key is None
    assert "prefix" in err


def test_blank_string_after_strip_falls_back():
    key, err = resolve_client_session_key("   ", platform="cli", chat_id="bob")
    assert err == ""
    assert key == "gateway:cli:bob"


@pytest.mark.asyncio
async def test_ws_auth_accepts_cli_session_key(gateway_ws_url):
    async with aiohttp.ClientSession() as s:
        async with s.ws_connect(gateway_ws_url) as ws:
            await ws.send_json({
                "type": "auth", "platform": "cli",
                "user_id": "alice", "session_key": "cli:alice",
            })
            msg = await ws.receive_json()
            assert msg["type"] == "auth_ok"
            assert msg["session_key"] == "cli:alice"


@pytest.mark.asyncio
async def test_ws_auth_rejects_impersonation(gateway_ws_url):
    async with aiohttp.ClientSession() as s:
        async with s.ws_connect(gateway_ws_url) as ws:
            await ws.send_json({
                "type": "auth", "platform": "cli",
                "user_id": "alice", "session_key": "gateway:wechat:victim",
            })
            msg = await ws.receive_json()
            assert msg["type"] == "error"

