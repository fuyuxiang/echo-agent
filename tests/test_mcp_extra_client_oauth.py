"""Extra tests for MCPClient (transport mocked) and MCPOAuthClient (aiohttp mocked)."""

from __future__ import annotations

import asyncio
import time
from unittest.mock import AsyncMock, MagicMock, patch

import pytest

from echo_agent.mcp.client import MCPClient
from echo_agent.mcp.oauth import MCPOAuthClient


def _make_transport():
    transport = MagicMock()
    transport.send = AsyncMock()
    transport.close = AsyncMock()
    transport.receive = AsyncMock()
    transport.is_connected = True
    return transport


# ===========================================================================
# MCPClient request/response plumbing
# ===========================================================================


class TestMCPClientRequest:
    @pytest.mark.asyncio
    async def test_request_resolves_via_read_loop(self):
        transport = _make_transport()
        client = MCPClient("srv", transport)

        # Drive the read loop manually by feeding one response then disconnect.
        async def _do():
            task = asyncio.ensure_future(client._request("tools/list", {}))
            await asyncio.sleep(0)  # let send happen, req_id assigned
            # Simulate the reader resolving the pending future.
            req_id = client._request_id
            client._pending[req_id].set_result({"tools": [{"name": "t"}]})
            return await task

        result = await _do()
        assert result["tools"][0]["name"] == "t"

    @pytest.mark.asyncio
    async def test_request_when_closed_raises(self):
        transport = _make_transport()
        client = MCPClient("srv", transport)
        client._closed = True
        with pytest.raises(ConnectionError):
            await client._request("x", {})

    @pytest.mark.asyncio
    async def test_request_timeout(self):
        transport = _make_transport()
        client = MCPClient("srv", transport)
        with pytest.raises(TimeoutError):
            await client._request("slow", {}, timeout=0.01)

    @pytest.mark.asyncio
    async def test_list_tools_parses_result(self):
        transport = _make_transport()
        client = MCPClient("srv", transport)
        client._request = AsyncMock(return_value={"tools": [{"name": "a"}, {"name": "b"}]})
        tools = await client.list_tools()
        assert [t["name"] for t in tools] == ["a", "b"]

    @pytest.mark.asyncio
    async def test_call_tool_passes_args(self):
        transport = _make_transport()
        client = MCPClient("srv", transport)
        client._request = AsyncMock(return_value={"content": []})
        await client.call_tool("search", {"q": "x"}, timeout=5)
        client._request.assert_awaited_once()
        args = client._request.call_args
        assert args.args[0] == "tools/call"
        assert args.args[1] == {"name": "search", "arguments": {"q": "x"}}

    @pytest.mark.asyncio
    async def test_list_resources_and_prompts(self):
        transport = _make_transport()
        client = MCPClient("srv", transport)
        client._request = AsyncMock(side_effect=[
            {"resources": [{"uri": "file:///x"}]},
            {"prompts": [{"name": "p"}]},
        ])
        res = await client.list_resources()
        prompts = await client.list_prompts()
        assert res[0]["uri"] == "file:///x"
        assert prompts[0]["name"] == "p"

    @pytest.mark.asyncio
    async def test_initialize_sets_server_info(self):
        transport = _make_transport()
        client = MCPClient("srv", transport)
        client._request = AsyncMock(return_value={
            "serverInfo": {"name": "TestSrv"},
            "capabilities": {"tools": {}},
        })
        client._notify = AsyncMock()
        await client.initialize()
        assert client._server_info["name"] == "TestSrv"
        assert "tools" in client._server_capabilities
        client._notify.assert_awaited_once()

    @pytest.mark.asyncio
    async def test_is_connected_property(self):
        transport = _make_transport()
        client = MCPClient("srv", transport)
        assert client.is_connected is True

    @pytest.mark.asyncio
    async def test_disconnect_cancels_pending(self):
        transport = _make_transport()
        client = MCPClient("srv", transport)
        fut = asyncio.get_running_loop().create_future()
        client._pending[1] = fut
        await client.disconnect()
        assert client._closed is True
        assert not client._pending
        transport.close.assert_awaited_once()

    @pytest.mark.asyncio
    async def test_connect_starts_reader_and_initializes(self):
        transport = _make_transport()
        transport.connect = AsyncMock()
        client = MCPClient("srv", transport)
        client.initialize = AsyncMock(return_value={})
        await client.connect(timeout=5)
        assert client._reader_task is not None
        transport.connect.assert_awaited_once()
        client.initialize.assert_awaited_once()
        await client.disconnect()

    @pytest.mark.asyncio
    async def test_read_resource_and_get_prompt(self):
        transport = _make_transport()
        client = MCPClient("srv", transport)
        client._request = AsyncMock(side_effect=[
            {"contents": [{"text": "data"}]},
            {"messages": [{"role": "user"}]},
        ])
        resource = await client.read_resource("file:///x")
        prompt = await client.get_prompt("p", {"a": 1})
        assert resource["contents"][0]["text"] == "data"
        assert prompt["messages"][0]["role"] == "user"

    @pytest.mark.asyncio
    async def test_notify_sends_without_id(self):
        transport = _make_transport()
        client = MCPClient("srv", transport)
        await client._notify("notifications/x", {"p": 1})
        sent = transport.send.call_args.args[0]
        assert sent["method"] == "notifications/x"
        assert "id" not in sent


class TestMCPClientReadLoop:
    @pytest.mark.asyncio
    async def test_read_loop_resolves_result(self):
        transport = _make_transport()
        client = MCPClient("srv", transport)
        fut = asyncio.get_running_loop().create_future()
        client._pending[7] = fut

        # First receive returns a result message, then signal disconnect.
        def _recv_seq():
            transport.is_connected = False
            return {"id": 7, "result": {"ok": True}}

        transport.receive = AsyncMock(side_effect=lambda: _recv_seq())
        await client._read_loop()
        assert fut.result() == {"ok": True}

    @pytest.mark.asyncio
    async def test_read_loop_sets_error(self):
        transport = _make_transport()
        client = MCPClient("srv", transport)
        fut = asyncio.get_running_loop().create_future()
        client._pending[3] = fut

        def _recv():
            transport.is_connected = False
            return {"id": 3, "error": {"code": -32000, "message": "bad"}}

        transport.receive = AsyncMock(side_effect=lambda: _recv())
        await client._read_loop()
        with pytest.raises(RuntimeError, match="bad"):
            fut.result()

    @pytest.mark.asyncio
    async def test_read_loop_queues_notification(self):
        transport = _make_transport()
        client = MCPClient("srv", transport)

        def _recv():
            transport.is_connected = False
            return {"method": "notifications/progress", "params": {"p": 1}}

        transport.receive = AsyncMock(side_effect=lambda: _recv())
        await client._read_loop()
        msg = client._notifications.get_nowait()
        assert msg["method"] == "notifications/progress"


# ===========================================================================
# MCPOAuthClient — aiohttp mocked
# ===========================================================================


def _aiohttp_session(resp):
    """Build a fake aiohttp ClientSession whose get/post return `resp`."""
    resp.__aenter__ = AsyncMock(return_value=resp)
    resp.__aexit__ = AsyncMock(return_value=False)
    session = MagicMock()
    session.get = MagicMock(return_value=resp)
    session.post = MagicMock(return_value=resp)
    session.__aenter__ = AsyncMock(return_value=session)
    session.__aexit__ = AsyncMock(return_value=False)
    return session


class TestMCPOAuthClient:
    def _make(self, tmp_path):
        return MCPOAuthClient("test_server", "https://mcp.example.com/", tmp_path)

    def test_get_access_token_none_when_no_file(self, tmp_path):
        client = self._make(tmp_path)
        assert client.get_access_token() is None

    def test_get_access_token_valid(self, tmp_path):
        client = self._make(tmp_path)
        client._save_token({
            "access_token": "tok", "expires_in": 3600, "obtained_at": time.time(),
        })
        assert client.get_access_token() == "tok"

    def test_get_access_token_expired(self, tmp_path):
        client = self._make(tmp_path)
        client._save_token({
            "access_token": "tok", "expires_in": 3600,
            "obtained_at": time.time() - 7200,
        })
        assert client.get_access_token() is None

    def test_find_free_port(self, tmp_path):
        client = self._make(tmp_path)
        port = client._find_free_port()
        assert isinstance(port, int)
        assert port > 0

    @pytest.mark.asyncio
    async def test_fetch_server_metadata_success(self, tmp_path):
        client = self._make(tmp_path)
        resp = MagicMock()
        resp.status = 200
        resp.json = AsyncMock(return_value={"token_endpoint": "https://x/token"})
        session = _aiohttp_session(resp)
        with patch("aiohttp.ClientSession", return_value=session):
            meta = await client._fetch_server_metadata()
        assert meta["token_endpoint"] == "https://x/token"

    @pytest.mark.asyncio
    async def test_fetch_server_metadata_error_returns_empty(self, tmp_path):
        client = self._make(tmp_path)
        resp = MagicMock()
        resp.status = 404
        resp.json = AsyncMock(return_value={})
        session = _aiohttp_session(resp)
        with patch("aiohttp.ClientSession", return_value=session):
            meta = await client._fetch_server_metadata()
        assert meta == {}

    @pytest.mark.asyncio
    async def test_register_client_returns_id(self, tmp_path):
        client = self._make(tmp_path)
        resp = MagicMock()
        resp.status = 201
        resp.json = AsyncMock(return_value={"client_id": "dyn-id"})
        session = _aiohttp_session(resp)
        with patch("aiohttp.ClientSession", return_value=session):
            cid = await client._register_client("https://x/register")
        assert cid == "dyn-id"

    @pytest.mark.asyncio
    async def test_exchange_code_success(self, tmp_path):
        client = self._make(tmp_path)
        resp = MagicMock()
        resp.status = 200
        resp.json = AsyncMock(return_value={"access_token": "tok", "expires_in": 3600})
        session = _aiohttp_session(resp)
        with patch("aiohttp.ClientSession", return_value=session):
            data = await client._exchange_code(
                "https://x/token", "code123", "cid", "verifier", "http://localhost/cb"
            )
        assert data["access_token"] == "tok"
        assert "obtained_at" in data

    @pytest.mark.asyncio
    async def test_exchange_code_failure_raises(self, tmp_path):
        client = self._make(tmp_path)
        resp = MagicMock()
        resp.status = 400
        resp.text = AsyncMock(return_value="invalid_grant")
        session = _aiohttp_session(resp)
        with patch("aiohttp.ClientSession", return_value=session):
            with pytest.raises(RuntimeError, match="Token exchange failed"):
                await client._exchange_code(
                    "https://x/token", "bad", "cid", "verifier", "http://localhost/cb"
                )

    @pytest.mark.asyncio
    async def test_refresh_token_success(self, tmp_path):
        client = self._make(tmp_path)
        resp = MagicMock()
        resp.status = 200
        resp.json = AsyncMock(return_value={"access_token": "new", "expires_in": 3600})
        session = _aiohttp_session(resp)
        with patch("aiohttp.ClientSession", return_value=session):
            new = await client._refresh_token({"refresh_token": "rt"})
        assert new["access_token"] == "new"
        assert new["refresh_token"] == "rt"  # carried over via setdefault

    @pytest.mark.asyncio
    async def test_ensure_token_refreshes_expired(self, tmp_path):
        client = self._make(tmp_path)
        client._save_token({
            "access_token": "old", "expires_in": 3600,
            "obtained_at": time.time() - 7200, "refresh_token": "rt",
        })
        client._refresh_token = AsyncMock(return_value={"access_token": "fresh"})
        result = await client.ensure_token()
        assert result == "fresh"

    @pytest.mark.asyncio
    async def test_ensure_token_authorizes_when_no_token(self, tmp_path):
        client = self._make(tmp_path)
        client._authorize = AsyncMock(return_value={"access_token": "authd"})
        result = await client.ensure_token()
        assert result == "authd"

    @pytest.mark.asyncio
    async def test_authorize_full_flow(self, tmp_path):
        client = self._make(tmp_path)
        client._fetch_server_metadata = AsyncMock(return_value={
            "authorization_endpoint": "https://x/auth",
            "token_endpoint": "https://x/token",
        })
        client._exchange_code = AsyncMock(return_value={
            "access_token": "final", "expires_in": 3600, "obtained_at": time.time(),
        })

        # Stub the callback server so no real socket is opened; resolve the
        # auth code future immediately.
        async def fake_start_server(port, state, future):
            future.set_result("authcode")
            server = MagicMock()
            server.close = MagicMock()
            server.wait_closed = AsyncMock()
            return server

        client._start_callback_server = fake_start_server
        with patch("webbrowser.open", return_value=True):
            data = await client._authorize()
        assert data["access_token"] == "final"
        # Token persisted to disk.
        assert client.get_access_token() == "final"

    @pytest.mark.asyncio
    async def test_authorize_with_dynamic_registration(self, tmp_path):
        client = self._make(tmp_path)
        client._fetch_server_metadata = AsyncMock(return_value={
            "authorization_endpoint": "https://x/auth",
            "token_endpoint": "https://x/token",
            "registration_endpoint": "https://x/register",
        })
        client._register_client = AsyncMock(return_value="dyn-client")
        client._exchange_code = AsyncMock(return_value={"access_token": "t"})

        async def fake_start_server(port, state, future):
            future.set_result("code")
            server = MagicMock()
            server.close = MagicMock()
            server.wait_closed = AsyncMock()
            return server

        client._start_callback_server = fake_start_server
        with patch("webbrowser.open", return_value=True):
            await client._authorize()
        client._register_client.assert_awaited_once()
        # client_id used in exchange should be the dynamic one.
        assert client._exchange_code.call_args.args[2] == "dyn-client"

    @pytest.mark.asyncio
    async def test_authorize_timeout(self, tmp_path):
        client = self._make(tmp_path)
        client._fetch_server_metadata = AsyncMock(return_value={})

        async def fake_start_server(port, state, future):
            server = MagicMock()
            server.close = MagicMock()
            server.wait_closed = AsyncMock()
            return server  # never resolves the future

        client._start_callback_server = fake_start_server
        with patch("webbrowser.open", return_value=True), \
             patch("asyncio.wait_for", AsyncMock(side_effect=asyncio.TimeoutError())):
            with pytest.raises(RuntimeError, match="timed out"):
                await client._authorize()


class TestOAuthCallbackServer:
    """Drive the real callback HTTP handler via a started server."""

    def _make(self, tmp_path):
        return MCPOAuthClient("test_server", "https://mcp.example.com/", tmp_path)

    @pytest.mark.asyncio
    async def test_callback_resolves_code_on_state_match(self, tmp_path):
        client = self._make(tmp_path)
        port = client._find_free_port()
        future: asyncio.Future = asyncio.get_running_loop().create_future()
        server = await client._start_callback_server(port, "expected-state", future)
        try:
            reader, writer = await asyncio.open_connection("127.0.0.1", port)
            writer.write(
                b"GET /callback?state=expected-state&code=the-code HTTP/1.1\r\n\r\n"
            )
            await writer.drain()
            await reader.read(4096)
            writer.close()
            code = await asyncio.wait_for(future, timeout=2)
            assert code == "the-code"
        finally:
            server.close()
            await server.wait_closed()

    @pytest.mark.asyncio
    async def test_callback_rejects_state_mismatch(self, tmp_path):
        client = self._make(tmp_path)
        port = client._find_free_port()
        future: asyncio.Future = asyncio.get_running_loop().create_future()
        server = await client._start_callback_server(port, "expected-state", future)
        try:
            reader, writer = await asyncio.open_connection("127.0.0.1", port)
            writer.write(b"GET /callback?state=wrong&code=x HTTP/1.1\r\n\r\n")
            await writer.drain()
            body = await reader.read(4096)
            writer.close()
            assert b"State mismatch" in body
            assert not future.done()
        finally:
            server.close()
            await server.wait_closed()
