"""Protocol boundary and lifecycle tests for MCP transports."""

from __future__ import annotations

import json
from unittest.mock import AsyncMock, MagicMock

import aiohttp
import pytest

from echo_agent.mcp.transport import (
    MCPUnauthorizedError,
    StdioTransport,
    StreamableHttpTransport,
    redact_argv,
    redact_text,
)


class _ChunkedContent:
    def __init__(self, chunks: list[bytes]):
        self._chunks = chunks

    def iter_any(self):
        return self

    def __aiter__(self):
        return self

    async def __anext__(self):
        if not self._chunks:
            raise StopAsyncIteration
        return self._chunks.pop(0)


def _response(*, status: int, headers: dict[str, str] | None = None, text: str = ""):
    response = MagicMock()
    response.status = status
    response.headers = headers or {}
    response.text = AsyncMock(return_value=text)
    return response


class _ResponseContext:
    def __init__(self, response):
        self.response = response
        self.exited = False

    async def __aenter__(self):
        return self.response

    async def __aexit__(self, exc_type, exc, tb):
        self.exited = True
        return False


@pytest.mark.asyncio
async def test_sse_incremental_utf8_decoder_preserves_split_codepoint() -> None:
    transport = StreamableHttpTransport("https://example.invalid/mcp")
    transport._connected = True
    message = {"jsonrpc": "2.0", "id": 1, "result": {"text": "你好"}}
    payload = ("data: " + json.dumps(message, ensure_ascii=False) + "\n\n").encode()
    first_character = payload.index("你".encode())
    response = MagicMock()
    response.content = _ChunkedContent(
        [payload[: first_character + 1], payload[first_character + 1 : first_character + 2], payload[first_character + 2 :]]
    )

    await transport._read_sse_response(response)

    assert await transport.receive() == message


@pytest.mark.asyncio
async def test_sse_trailing_event_and_buffer_ceiling(monkeypatch: pytest.MonkeyPatch) -> None:
    from echo_agent.mcp import transport as transport_module

    transport = StreamableHttpTransport("https://example.invalid/mcp")
    transport._connected = True
    response = MagicMock()
    response.content = _ChunkedContent([b'data: {"id": 7}'])
    await transport._read_sse_response(response)
    assert await transport.receive() == {"id": 7}

    monkeypatch.setattr(transport_module, "_MAX_SSE_BUFFER_BYTES", 8)
    response.content = _ChunkedContent([b"123456789"])
    with pytest.raises(ConnectionError, match="buffer limit"):
        await transport._read_sse_response(response)


@pytest.mark.asyncio
async def test_http_connect_replaces_stale_pool_and_is_idempotent(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    from echo_agent.mcp import transport as transport_module

    stale = MagicMock()
    stale.close = AsyncMock()
    current = MagicMock()
    current.close = AsyncMock()
    factory = MagicMock(return_value=current)
    monkeypatch.setattr(transport_module.aiohttp, "ClientSession", factory)

    transport = StreamableHttpTransport("https://example.invalid/mcp")
    transport._session = stale
    transport._auth_failed = True
    await transport.connect(timeout=9)
    await transport.connect(timeout=9)

    stale.close.assert_awaited_once_with()
    factory.assert_called_once()
    assert transport.is_connected is True
    assert transport.auth_failed is False
    await transport.close()
    current.close.assert_awaited_once_with()


@pytest.mark.asyncio
async def test_notification_stream_requires_connection_and_propagates_auth_state() -> None:
    transport = StreamableHttpTransport("https://example.invalid/mcp")
    with pytest.raises(ConnectionError, match="not connected"):
        await transport.open_notification_stream()

    response = _response(
        status=401,
        headers={"WWW-Authenticate": 'Bearer resource_metadata="https://auth.invalid"'},
        text="revoked",
    )
    session = MagicMock()
    session.get.return_value = _ResponseContext(response)
    transport._session = session
    transport._connected = True

    await transport._listen_get_stream()

    assert transport.auth_failed is True
    assert transport.is_connected is False


@pytest.mark.asyncio
async def test_notification_stream_405_is_supported_noop() -> None:
    transport = StreamableHttpTransport("https://example.invalid/mcp")
    session = MagicMock()
    session.get.return_value = _ResponseContext(_response(status=405))
    transport._session = session
    transport._connected = True

    await transport._listen_get_stream()

    assert transport.is_connected is True


@pytest.mark.asyncio
async def test_notification_stream_eof_marks_transport_disconnected() -> None:
    transport = StreamableHttpTransport("https://example.invalid/mcp")
    response = _response(status=200, headers={"Content-Type": "text/event-stream"})
    response.content = _ChunkedContent([])
    session = MagicMock()
    session.get.return_value = _ResponseContext(response)
    transport._session = session
    transport._connected = True

    await transport._listen_get_stream()

    assert transport.is_connected is False


@pytest.mark.asyncio
async def test_post_sse_failure_marks_transport_disconnected_and_closes_response() -> None:
    transport = StreamableHttpTransport("https://example.invalid/mcp")
    transport._connected = True
    transport._read_sse_response = AsyncMock(side_effect=OSError("stream reset"))
    context = _ResponseContext(MagicMock())

    await transport._consume_sse_response(context, context.response)

    assert context.exited is True
    assert transport.is_connected is False


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "payload",
    [b"data: not-json\n\n", b"data: {\"text\":\"\xff\"}\n\n"],
)
async def test_post_sse_invalid_json_or_utf8_marks_transport_disconnected(
    payload: bytes,
) -> None:
    transport = StreamableHttpTransport("https://example.invalid/mcp")
    transport._connected = True
    response = MagicMock()
    response.content = _ChunkedContent([payload])
    context = _ResponseContext(response)

    await transport._consume_sse_response(context, response)

    assert transport.is_connected is False


@pytest.mark.asyncio
async def test_post_network_failure_marks_transport_disconnected() -> None:
    transport = StreamableHttpTransport("https://example.invalid/mcp")
    session = MagicMock()
    context = _ResponseContext(MagicMock())
    context.__aenter__ = AsyncMock(
        side_effect=aiohttp.ClientConnectionError("connection reset"),
    )
    session.post.return_value = context
    transport._session = session
    transport._connected = True

    with pytest.raises(aiohttp.ClientConnectionError):
        await transport.send({"jsonrpc": "2.0", "id": 1, "method": "ping"})

    assert transport.is_connected is False


@pytest.mark.asyncio
async def test_send_refuses_stale_session_after_stream_marked_disconnected() -> None:
    transport = StreamableHttpTransport("https://example.invalid/mcp")
    session = MagicMock()
    transport._session = session
    transport._connected = False

    with pytest.raises(ConnectionError, match="not connected"):
        await transport.send({"jsonrpc": "2.0", "id": 1, "method": "ping"})

    session.post.assert_not_called()


@pytest.mark.asyncio
async def test_malformed_json_response_marks_transport_disconnected() -> None:
    transport = StreamableHttpTransport("https://example.invalid/mcp")
    response = _response(
        status=200, headers={"Content-Type": "application/json"},
    )
    response.json = AsyncMock(side_effect=json.JSONDecodeError("bad", "x", 0))
    context = _ResponseContext(response)
    session = MagicMock()
    session.post.return_value = context
    transport._session = session
    transport._connected = True

    with pytest.raises(ConnectionError, match="malformed JSON"):
        await transport.send({"jsonrpc": "2.0", "id": 1, "method": "ping"})

    assert context.exited is True
    assert transport.is_connected is False


@pytest.mark.asyncio
async def test_receive_fails_fast_after_close_but_drains_already_queued_response() -> None:
    transport = StreamableHttpTransport("https://example.invalid/mcp")
    with pytest.raises(ConnectionError, match="not connected"):
        await transport.receive()

    await transport._response_queue.put({"id": 1})
    assert await transport.receive() == {"id": 1}


@pytest.mark.asyncio
async def test_json_body_queues_batch_and_ignores_non_objects() -> None:
    transport = StreamableHttpTransport("https://example.invalid/mcp")
    transport._connected = True
    response = MagicMock()
    response.json = AsyncMock(return_value=[{"id": 1}, "invalid", {"id": 2}])

    await transport._queue_json_body(response)

    assert await transport.receive() == {"id": 1}
    assert await transport.receive() == {"id": 2}


@pytest.mark.asyncio
async def test_close_clears_session_reference_when_pool_close_fails() -> None:
    transport = StreamableHttpTransport("https://example.invalid/mcp")
    session = MagicMock()
    session.close = AsyncMock(side_effect=RuntimeError("pool stuck"))
    transport._session = session
    transport._connected = True

    await transport.close()

    assert transport._session is None
    assert transport.is_connected is False


@pytest.mark.asyncio
async def test_http_status_classification_and_body_read_failure() -> None:
    transport = StreamableHttpTransport("https://example.invalid/mcp")
    assert await transport._check_status(_response(status=202), {"method": "ping"}) is True

    forbidden = _response(
        status=403,
        headers={"WWW-Authenticate": "Bearer realm=test"},
        text="forbidden",
    )
    with pytest.raises(MCPUnauthorizedError) as caught:
        await transport._check_status(forbidden, {"method": "tools/list"})
    assert caught.value.www_authenticate == "Bearer realm=test"

    broken = _response(status=500)
    broken.text.side_effect = RuntimeError("body already consumed")
    with pytest.raises(ConnectionError, match="HTTP error 500"):
        await transport._check_status(broken, {"method": "tools/call"})


def test_stdio_secret_redaction_covers_separate_and_inline_flags() -> None:
    rendered = redact_argv(
        "server",
        ["--api-key", "sk-secret-value", "--token=abc123456", "--port", "8080"],
    )
    assert "sk-secret-value" not in rendered
    assert "abc123456" not in rendered
    assert rendered == "server --api-key *** --token=*** --port 8080"
    assert "super-secret" not in redact_text("access_token=super-secret")


@pytest.mark.asyncio
async def test_stdio_send_connection_and_broken_pipe_errors() -> None:
    transport = StdioTransport("server")
    with pytest.raises(ConnectionError, match="not connected"):
        await transport.send({"id": 1})

    process = MagicMock()
    process.returncode = 9
    process.stdin = MagicMock()
    transport._process = process
    with pytest.raises(ConnectionError, match="exited with code 9"):
        await transport.send({"id": 1})

    process.returncode = None
    process.stdin.write.side_effect = BrokenPipeError("closed")
    with pytest.raises(ConnectionError, match="closed its stdin"):
        await transport.send({"id": 1})


@pytest.mark.asyncio
async def test_stdio_receive_skips_banner_blank_and_returns_json() -> None:
    transport = StdioTransport("server")
    process = MagicMock()
    process.returncode = None
    process.stdin = MagicMock()
    process.stdout.readline = AsyncMock(
        side_effect=[b"server banner\n", b"\n", b'{"jsonrpc":"2.0","id":4}\n']
    )
    transport._process = process

    assert await transport.receive() == {"jsonrpc": "2.0", "id": 4}


@pytest.mark.asyncio
async def test_stdio_receive_reports_oversize_and_eof() -> None:
    transport = StdioTransport("server")
    process = MagicMock()
    process.stdout.readline = AsyncMock(side_effect=ValueError("Separator is not found"))
    transport._process = process
    with pytest.raises(ConnectionError, match="oversized line"):
        await transport.receive()

    process.stdout.readline = AsyncMock(return_value=b"")
    with pytest.raises(ConnectionError, match="closed"):
        await transport.receive()
