import pytest

from echo_agent.cli.attach_client import (
    AuthError,
    NoGatewayError,
    OutboundRenderer,
    authenticate,
    build_ws_url,
    connect_ws,
)


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


def test_renderer_streams_then_finalizes_without_dup(capsys):
    r = OutboundRenderer()
    r.render({
        "type": "message", "text": "你好", "is_final": False,
        "metadata": {"_token_stream": True, "_inbound_event_id": "e1"},
    })
    r.render({
        "type": "message", "text": "，世界", "is_final": False,
        "metadata": {"_token_stream": True, "_inbound_event_id": "e1"},
    })
    r.render({
        "type": "message", "text": "你好，世界", "is_final": True,
        "message_kind": "final",
        "metadata": {"_token_stream": True, "_inbound_event_id": "e1"},
    })
    out = capsys.readouterr().out
    # 流式两段 + 最终只补余下，全文 "你好，世界" 只出现一次
    assert out.count("你好，世界") == 1


def test_renderer_non_stream_prints_text(capsys):
    r = OutboundRenderer()
    r.render({"type": "message", "text": "完整回复", "is_final": True, "metadata": {}})
    assert "完整回复" in capsys.readouterr().out


class _FakeWS:
    """Minimal ws stub: records sent frames, replays a queued receive_json."""

    def __init__(self, reply: dict) -> None:
        self.sent: list[dict] = []
        self._reply = reply

    async def send_json(self, data: dict) -> None:
        self.sent.append(data)

    async def receive_json(self) -> dict:
        return self._reply


@pytest.mark.asyncio
async def test_authenticate_returns_session_key_on_ok():
    ws = _FakeWS({"type": "auth_ok", "session_key": "sk-server"})
    key = await authenticate(
        ws, platform="cli", user_id="u1", session_key="sk-client", token="t"
    )
    assert key == "sk-server"
    assert ws.sent[0]["type"] == "auth"
    assert ws.sent[0]["platform"] == "cli"


@pytest.mark.asyncio
async def test_authenticate_raises_on_error():
    ws = _FakeWS({"type": "auth_error", "error": "bad token"})
    with pytest.raises(AuthError) as ei:
        await authenticate(
            ws, platform="cli", user_id="u1", session_key="sk", token="t"
        )
    assert "bad token" in str(ei.value)
