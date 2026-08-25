"""Extra tests for MCPClient (transport mocked) and MCPOAuthClient (aiohttp mocked)."""

from __future__ import annotations

import asyncio
import json
import time
from unittest.mock import AsyncMock, MagicMock, patch

import pytest

from echo_agent.mcp.client import MCPClient
from echo_agent.mcp.oauth import MCPOAuthClient, MCPOAuthError


def _make_transport():
    transport = MagicMock()
    transport.send = AsyncMock()
    transport.close = AsyncMock()
    transport.receive = AsyncMock()
    transport.is_connected = True
    return transport


def _connected_client(name="srv", transport=None):
    """An MCPClient in the state `connect()` would leave it in.

    `is_connected` now requires a live read loop, not just a live transport: a
    reader that had died used to leave the client looking healthy while every
    request ran to its full timeout. Tests that construct the client directly
    have to opt into "the reader is running" the way connect() does.
    """
    client = MCPClient(name, transport if transport is not None else _make_transport())
    client._reader_alive = True
    return client


# ===========================================================================
# MCPClient request/response plumbing
# ===========================================================================


class TestMCPClientRequest:
    @pytest.mark.asyncio
    async def test_request_resolves_via_read_loop(self):
        transport = _make_transport()
        client = _connected_client(transport=transport)

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
        client = _connected_client()
        with pytest.raises(TimeoutError):
            await client._request("slow", {}, timeout=0.01)

    @pytest.mark.asyncio
    async def test_request_fails_fast_when_reader_is_dead(self):
        """A dead read loop must be reported immediately, not after the full
        timeout. One malformed frame used to kill the reader while
        `is_connected` still said True, turning every later call into a 120s
        wait that never recovered."""
        client = MCPClient("srv", _make_transport())  # reader never started
        with pytest.raises(ConnectionError, match="no live reader"):
            await client._request("tools/call", {}, timeout=99)

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
        client = _connected_client()
        assert client.is_connected is True

    @pytest.mark.asyncio
    async def test_is_connected_false_when_reader_dead(self):
        """A live transport is not sufficient — the reader has to be running for
        a request to ever complete."""
        transport = _make_transport()
        client = MCPClient("srv", transport)
        assert transport.is_connected is True
        assert client.is_connected is False

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


def client_for(tmp_path):
    """A real (unmocked) OAuth client pointed at a loopback MCP server.

    Loopback is what makes plaintext http:// acceptable to
    ``require_secure_endpoint``, so the redirect tests can run against local
    aiohttp servers instead of mocks.
    """
    return MCPOAuthClient("redirect_probe", "http://127.0.0.1:9/", tmp_path)


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

    def test_rejects_server_name_that_escapes_the_token_dir(self, tmp_path):
        """`server_name` becomes a filename, and "../../escaped" wrote the
        token outside the credential directory entirely."""
        for bad in ("../../escaped", "a/b", "", ".hidden"):
            with pytest.raises(MCPOAuthError):
                MCPOAuthClient(bad, "https://mcp.example.com", tmp_path)

    def test_token_file_is_not_world_readable(self, tmp_path):
        """Tokens were written by `write_text`, landing at 0644 — every local
        user could read the access and refresh tokens."""
        import stat

        client = self._make(tmp_path)
        client._save_token({"access_token": "sk-secret", "expires_in": 3600,
                            "obtained_at": time.time()})
        assert stat.S_IMODE(client._token_file.stat().st_mode) == 0o600
        assert stat.S_IMODE(tmp_path.stat().st_mode) == 0o700

    def test_bind_callback_socket_is_listening(self, tmp_path):
        """Bound once and handed to the server, rather than probed-then-rebound
        (which left a window for another process to take the port)."""
        client = self._make(tmp_path)
        sock = client._bind_callback_socket()
        try:
            assert sock.getsockname()[1] > 0
        finally:
            sock.close()

    @pytest.mark.asyncio
    async def test_discovery_looks_at_the_origin_not_the_mcp_path(self, tmp_path):
        """Metadata lives at the origin. The old code appended the well-known
        path to the full MCP URL, so discovery never found anything."""
        client = MCPOAuthClient("srv", "https://mcp.example.com/mcp", tmp_path)
        url = client._well_known("https://mcp.example.com/mcp", "oauth-authorization-server")
        assert url.startswith("https://mcp.example.com/.well-known/oauth-authorization-server")

    @pytest.mark.asyncio
    async def test_discover_protected_resource_success(self, tmp_path):
        client = self._make(tmp_path)
        resp = MagicMock()
        resp.status = 200
        resp.json = AsyncMock(return_value={"authorization_servers": ["https://as.example.com"]})
        session = _aiohttp_session(resp)
        with patch("aiohttp.ClientSession", return_value=session):
            meta = await client.discover_protected_resource()
        assert meta["authorization_servers"] == ["https://as.example.com"]

    @pytest.mark.asyncio
    async def test_discovery_returns_empty_on_error(self, tmp_path):
        client = self._make(tmp_path)
        resp = MagicMock()
        resp.status = 404
        resp.json = AsyncMock(return_value={})
        session = _aiohttp_session(resp)
        with patch("aiohttp.ClientSession", return_value=session):
            assert await client.discover_protected_resource() == {}

    def test_parse_www_authenticate_extracts_resource_metadata(self, tmp_path):
        client = self._make(tmp_path)
        params = client.parse_www_authenticate(
            'Bearer error="invalid_token", '
            'resource_metadata="https://x/.well-known/oauth-protected-resource"'
        )
        assert params["resource_metadata"].endswith("oauth-protected-resource")
        assert params["error"] == "invalid_token"

    def test_cross_origin_token_endpoint_is_refused(self, tmp_path):
        """The most serious problem in the old file: `token_endpoint` came from a
        network-fetched metadata document and was trusted unconditionally, so a
        hostile response could redirect the authorization code and PKCE verifier
        to an attacker's origin."""
        client = self._make(tmp_path)
        with pytest.raises(MCPOAuthError, match="not the issuer's origin"):
            client._endpoint_from(
                {"token_endpoint": "https://attacker.example.com/token"},
                "token_endpoint", "https://as.example.com", "/token",
            )

    def test_same_origin_endpoint_and_default_are_accepted(self, tmp_path):
        client = self._make(tmp_path)
        assert client._endpoint_from(
            {"token_endpoint": "https://as.example.com/tok"},
            "token_endpoint", "https://as.example.com", "/token",
        ) == "https://as.example.com/tok"
        assert client._endpoint_from(
            {}, "token_endpoint", "https://as.example.com", "/token",
        ) == "https://as.example.com/token"

    def test_plaintext_endpoint_refused_unless_loopback(self, tmp_path):
        from echo_agent.mcp.oauth import require_secure_endpoint

        with pytest.raises(MCPOAuthError):
            require_secure_endpoint("http://evil.example.com/token", "token_endpoint")
        # Loopback is exempt so a locally-run AS still works.
        assert require_secure_endpoint("http://127.0.0.1:9000/token", "token_endpoint")

    @pytest.mark.asyncio
    async def test_register_client_returns_id_and_persists(self, tmp_path):
        """Registration results are persisted; without that every restart
        re-registered and lost any issued client secret."""
        client = self._make(tmp_path)
        resp = MagicMock()
        resp.status = 201
        resp.json = AsyncMock(return_value={"client_id": "dyn-id", "client_secret": "shh"})
        session = _aiohttp_session(resp)
        with patch("aiohttp.ClientSession", return_value=session):
            cid, secret = await client._register_client(
                "https://as.example.com/register",
                "http://127.0.0.1:5555/callback",
                "https://as.example.com",
                "https://as.example.com/token",
            )
        assert (cid, secret) == ("dyn-id", "shh")
        assert client._load_client_credentials() == ("dyn-id", "shh")

    # ── persisted client credentials are bound to where they were issued ────
    #
    # The record is keyed only by server *name*, which is a config key the
    # operator can repoint. Loading it unconditionally meant a secret issued by
    # one AS could be sent to whoever the config named next — a credential leak
    # triggered by an ordinary config edit.

    def _register(self, client, *, issuer, token_endpoint, server_url=None):
        """Persist a registration record as _register_client would."""
        client._save_client_credentials({
            "client_id": "dyn-id",
            "client_secret": "shh",
            "issuer": issuer,
            "token_endpoint": token_endpoint,
            "server_url": server_url if server_url is not None else client._server_url,
            "redirect_uri": "http://127.0.0.1:5555/callback",
            "registered_at": time.time(),
        })

    def test_matching_registration_is_reused(self, tmp_path):
        client = self._make(tmp_path)
        self._register(
            client,
            issuer="https://as.example.com",
            token_endpoint="https://as.example.com/token",
        )
        assert client._load_client_credentials(
            issuer="https://as.example.com",
            token_endpoint="https://as.example.com/token",
        ) == ("dyn-id", "shh")

    def test_secret_is_not_reused_for_a_different_issuer(self, tmp_path):
        client = self._make(tmp_path)
        self._register(
            client,
            issuer="https://as.example.com",
            token_endpoint="https://as.example.com/token",
        )
        assert client._load_client_credentials(
            issuer="https://attacker.example.com",
            token_endpoint="https://attacker.example.com/token",
        ) == ("", "")
        # Discarded, not merely withheld: the next attempt must re-register.
        assert not client._client_file.exists()

    def test_secret_is_not_reused_for_a_different_token_endpoint(self, tmp_path):
        client = self._make(tmp_path)
        self._register(
            client,
            issuer="https://as.example.com",
            token_endpoint="https://as.example.com/token",
        )
        assert client._load_client_credentials(
            issuer="https://as.example.com",
            token_endpoint="https://elsewhere.example.com/token",
        ) == ("", "")

    def test_secret_is_not_reused_after_the_server_url_changes(self, tmp_path):
        """The exact scenario: same config key, repointed at another address."""
        client = self._make(tmp_path)
        self._register(
            client,
            issuer="https://as.example.com",
            token_endpoint="https://as.example.com/token",
            server_url="https://old-mcp.example.com",
        )
        assert client._load_client_credentials(
            issuer="https://as.example.com",
            token_endpoint="https://as.example.com/token",
        ) == ("", "")

    def test_pre_binding_record_is_migrated_not_trusted(self, tmp_path):
        """Old records carry no server_url, so their scope is unknowable."""
        client = self._make(tmp_path)
        client._save_client_credentials({"client_id": "old-id", "client_secret": "old-shh"})

        assert client._load_client_credentials(
            issuer="https://as.example.com",
            token_endpoint="https://as.example.com/token",
        ) == ("", "")
        assert not client._client_file.exists()

    def test_unbound_load_still_returns_the_record(self, tmp_path):
        """Callers that pass no expectation get the record as-is."""
        client = self._make(tmp_path)
        client._save_client_credentials({"client_id": "c", "client_secret": "s"})
        assert client._load_client_credentials() == ("c", "s")

    @pytest.mark.asyncio
    async def test_refresh_does_not_send_a_secret_to_a_new_endpoint(self, tmp_path):
        """The refresh POST carries the client secret; it must not travel to a
        token endpoint other than the one that issued it."""
        client = self._make(tmp_path)
        self._register(
            client,
            issuer="https://as.example.com",
            token_endpoint="https://as.example.com/token",
        )
        resp = MagicMock()
        resp.status = 200
        resp.json = AsyncMock(return_value={"access_token": "new"})
        session = _aiohttp_session(resp)
        with patch("aiohttp.ClientSession", return_value=session):
            await client._refresh_token({
                "refresh_token": "rt",
                "issuer": "https://other.example.com",
                "token_endpoint": "https://other.example.com/token",
            })
        body = session.post.call_args.kwargs["data"]
        assert "client_secret" not in body

    @pytest.mark.asyncio
    async def test_refresh_sends_the_secret_to_its_own_endpoint(self, tmp_path):
        client = self._make(tmp_path)
        self._register(
            client,
            issuer="https://as.example.com",
            token_endpoint="https://as.example.com/token",
        )
        resp = MagicMock()
        resp.status = 200
        resp.json = AsyncMock(return_value={"access_token": "new"})
        session = _aiohttp_session(resp)
        with patch("aiohttp.ClientSession", return_value=session):
            await client._refresh_token({
                "refresh_token": "rt",
                "issuer": "https://as.example.com",
                "token_endpoint": "https://as.example.com/token",
            })
        assert session.post.call_args.kwargs["data"]["client_secret"] == "shh"

    @pytest.mark.asyncio
    async def test_registration_records_the_server_url(self, tmp_path):
        client = self._make(tmp_path)
        resp = MagicMock()
        resp.status = 201
        resp.json = AsyncMock(return_value={"client_id": "c", "client_secret": "s"})
        session = _aiohttp_session(resp)
        with patch("aiohttp.ClientSession", return_value=session):
            await client._register_client(
                "https://as.example.com/register", "http://127.0.0.1:5555/callback",
                "https://as.example.com", "https://as.example.com/token",
            )
        record = json.loads(client._client_file.read_text())
        assert record["server_url"] == "https://mcp.example.com"

    @pytest.mark.asyncio
    async def test_register_client_sends_the_exact_redirect_uri(self, tmp_path):
        """It registered "http://localhost/callback" (no port) while redirecting
        to a random port — an exact-match failure at any strict AS."""
        client = self._make(tmp_path)
        resp = MagicMock()
        resp.status = 201
        resp.json = AsyncMock(return_value={"client_id": "c"})
        session = _aiohttp_session(resp)
        with patch("aiohttp.ClientSession", return_value=session):
            await client._register_client(
                "https://as.example.com/register", "http://127.0.0.1:5555/callback",
                "https://as.example.com", "https://as.example.com/token",
            )
        body = session.post.call_args.kwargs["json"]
        assert body["redirect_uris"] == ["http://127.0.0.1:5555/callback"]

    @pytest.mark.asyncio
    async def test_exchange_code_success(self, tmp_path):
        client = self._make(tmp_path)
        resp = MagicMock()
        resp.status = 200
        resp.json = AsyncMock(return_value={"access_token": "tok", "expires_in": 3600})
        session = _aiohttp_session(resp)
        with patch("aiohttp.ClientSession", return_value=session):
            data = await client._exchange_code(
                "https://as.example.com/token", "code123", "cid", "",
                "verifier", "http://127.0.0.1:5555/callback",
            )
        assert data["access_token"] == "tok"
        assert "obtained_at" in data

    @pytest.mark.asyncio
    async def test_exchange_code_sends_resource_parameter(self, tmp_path):
        """MCP requires RFC 8707 `resource` on token requests so the issued token
        is audienced to this server rather than usable anywhere."""
        client = self._make(tmp_path)
        resp = MagicMock()
        resp.status = 200
        resp.json = AsyncMock(return_value={"access_token": "tok"})
        session = _aiohttp_session(resp)
        with patch("aiohttp.ClientSession", return_value=session):
            await client._exchange_code(
                "https://as.example.com/token", "c", "cid", "",
                "verifier", "http://127.0.0.1:5555/callback",
            )
        body = session.post.call_args.kwargs["data"]
        assert body["resource"] == "https://mcp.example.com"
        assert body["code_verifier"] == "verifier"

    @pytest.mark.asyncio
    async def test_exchange_code_failure_raises(self, tmp_path):
        client = self._make(tmp_path)
        resp = MagicMock()
        resp.status = 400
        resp.text = AsyncMock(return_value="invalid_grant")
        session = _aiohttp_session(resp)
        with patch("aiohttp.ClientSession", return_value=session):
            with pytest.raises(MCPOAuthError, match="Token exchange failed"):
                await client._exchange_code(
                    "https://as.example.com/token", "bad", "cid", "",
                    "verifier", "http://127.0.0.1:5555/callback",
                )

    @pytest.mark.asyncio
    async def test_refresh_token_success(self, tmp_path):
        client = self._make(tmp_path)
        resp = MagicMock()
        resp.status = 200
        resp.json = AsyncMock(return_value={"access_token": "new", "expires_in": 3600})
        session = _aiohttp_session(resp)
        with patch("aiohttp.ClientSession", return_value=session):
            new = await client._refresh_token({
                "refresh_token": "rt",
                "issuer": "https://as.example.com",
                "token_endpoint": "https://as.example.com/token",
            })
        assert new["access_token"] == "new"
        assert new["refresh_token"] == "rt"  # carried over via setdefault

    @pytest.mark.asyncio
    async def test_refresh_sends_client_id_and_resource(self, tmp_path):
        """Both were missing. Most authorization servers reject a public-client
        refresh that does not identify the client."""
        client = self._make(tmp_path)
        resp = MagicMock()
        resp.status = 200
        resp.json = AsyncMock(return_value={"access_token": "new"})
        session = _aiohttp_session(resp)
        with patch("aiohttp.ClientSession", return_value=session):
            await client._refresh_token({
                "refresh_token": "rt",
                "issuer": "https://as.example.com",
                "token_endpoint": "https://as.example.com/token",
            })
        body = session.post.call_args.kwargs["data"]
        assert body["client_id"]
        assert body["resource"] == "https://mcp.example.com"

    @pytest.mark.asyncio
    async def test_refresh_refuses_cross_origin_stored_endpoint(self, tmp_path):
        client = self._make(tmp_path)
        assert await client._refresh_token({
            "refresh_token": "rt",
            "issuer": "https://as.example.com",
            "token_endpoint": "https://attacker.example.com/token",
        }) is None

    # ── credential POSTs must not follow redirects ──────────────────────────
    #
    # aiohttp follows redirects by default and re-sends the body on 307/308. The
    # same-origin validation covers the *initial* endpoint only, so without
    # allow_redirects=False a hostile token endpoint could answer
    # `307 Location: https://attacker/` and have the authorization code, PKCE
    # verifier, refresh token or client secret delivered to another origin.

    @pytest.mark.asyncio
    async def test_exchange_code_does_not_follow_redirects(self, tmp_path):
        client = self._make(tmp_path)
        resp = MagicMock()
        resp.status = 200
        resp.json = AsyncMock(return_value={"access_token": "tok"})
        session = _aiohttp_session(resp)
        with patch("aiohttp.ClientSession", return_value=session):
            await client._exchange_code(
                "https://as.example.com/token", "c", "cid", "",
                "verifier", "http://127.0.0.1:5555/callback",
            )
        assert session.post.call_args.kwargs["allow_redirects"] is False

    @pytest.mark.asyncio
    async def test_refresh_does_not_follow_redirects(self, tmp_path):
        client = self._make(tmp_path)
        resp = MagicMock()
        resp.status = 200
        resp.json = AsyncMock(return_value={"access_token": "new"})
        session = _aiohttp_session(resp)
        with patch("aiohttp.ClientSession", return_value=session):
            await client._refresh_token({
                "refresh_token": "rt",
                "issuer": "https://as.example.com",
                "token_endpoint": "https://as.example.com/token",
            })
        assert session.post.call_args.kwargs["allow_redirects"] is False

    @pytest.mark.asyncio
    async def test_registration_does_not_follow_redirects(self, tmp_path):
        client = self._make(tmp_path)
        resp = MagicMock()
        resp.status = 201
        resp.json = AsyncMock(return_value={"client_id": "c"})
        session = _aiohttp_session(resp)
        with patch("aiohttp.ClientSession", return_value=session):
            await client._register_client(
                "https://as.example.com/register", "http://127.0.0.1:5555/callback",
                "https://as.example.com", "https://as.example.com/token",
            )
        assert session.post.call_args.kwargs["allow_redirects"] is False

    @pytest.mark.asyncio
    async def test_cross_origin_307_never_reaches_the_second_server(self, tmp_path):
        """End-to-end over a real aiohttp client against two local servers.

        The first answers 307 pointing at the second. With redirect-following on,
        aiohttp replays the POST *body* — code, verifier, secret — at the second
        origin. The assertion is that the second server records nothing.
        """
        from aiohttp import web

        received: list[dict[str, str]] = []

        async def attacker(request: web.Request) -> web.Response:
            received.append(dict(await request.post()))
            return web.json_response({"access_token": "attacker-issued"})

        attacker_app = web.Application()
        attacker_app.router.add_post("/token", attacker)
        attacker_runner = web.AppRunner(attacker_app)
        await attacker_runner.setup()
        attacker_site = web.TCPSite(attacker_runner, "127.0.0.1", 0)
        await attacker_site.start()
        attacker_port = attacker_runner.addresses[0][1]

        async def redirector(request: web.Request) -> web.Response:
            raise web.HTTPTemporaryRedirect(
                location=f"http://127.0.0.1:{attacker_port}/token",
            )

        as_app = web.Application()
        as_app.router.add_post("/token", redirector)
        as_runner = web.AppRunner(as_app)
        await as_runner.setup()
        as_site = web.TCPSite(as_runner, "127.0.0.1", 0)
        await as_site.start()
        as_port = as_runner.addresses[0][1]

        try:
            token_endpoint = f"http://127.0.0.1:{as_port}/token"
            with pytest.raises(MCPOAuthError):
                await client_for(tmp_path)._exchange_code(
                    token_endpoint, "secret-code", "cid", "client-secret",
                    "secret-verifier", "http://127.0.0.1:5555/callback",
                )
            assert received == [], "credentials were forwarded to the redirect target"

            # And the refresh path, whose token is the longer-lived credential.
            assert await client_for(tmp_path)._refresh_token({
                "refresh_token": "secret-refresh",
                "issuer": f"http://127.0.0.1:{as_port}",
                "token_endpoint": token_endpoint,
            }) is None
            assert received == []
        finally:
            await as_runner.cleanup()
            await attacker_runner.cleanup()

    @pytest.mark.asyncio
    async def test_refresh_after_401_without_refresh_token(self, tmp_path):
        client = self._make(tmp_path)
        client._save_token({"access_token": "a", "expires_in": 3600,
                            "obtained_at": time.time()})
        assert await client.refresh_after_401() is None

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

    def _stub_auth_server(self, client, **extra):
        """Point discovery at a single fake AS on one origin."""
        metadata = {
            "authorization_endpoint": "https://as.example.com/auth",
            "token_endpoint": "https://as.example.com/token",
            **extra,
        }
        client._resolve_auth_server = AsyncMock(
            return_value=("https://as.example.com", metadata)
        )

    async def _drive_callback(self, client, code="authcode"):
        """Complete the loopback callback the way a browser would.

        The real socket is used rather than stubbed: the callback handler and the
        bind-once port handling are exactly the parts worth exercising.

        The authorization URL is parsed with the query-string machinery rather
        than a regex, because `redirect_uri` is percent-encoded in it — which is
        the point of building the URL with urlencode.
        """
        from urllib.parse import parse_qs, urlparse as _urlparse

        async def opener(url):
            params = parse_qs(_urlparse(url).query)
            redirect = params["redirect_uri"][0]
            port = _urlparse(redirect).port
            state = params["state"][0]
            for _ in range(50):
                try:
                    reader, writer = await asyncio.open_connection("127.0.0.1", port)
                except OSError:
                    await asyncio.sleep(0.02)
                    continue
                writer.write(
                    f"GET /callback?state={state}&code={code} HTTP/1.1\r\n"
                    f"Host: localhost\r\n\r\n".encode()
                )
                await writer.drain()
                await reader.read(4096)
                writer.close()
                return True
            return False

        def fake_open(url):
            asyncio.get_running_loop().create_task(opener(url))
            return True

        return fake_open

    @pytest.mark.asyncio
    async def test_authorize_full_flow(self, tmp_path):
        client = self._make(tmp_path)
        self._stub_auth_server(client)
        client._exchange_code = AsyncMock(return_value={
            "access_token": "final", "expires_in": 3600, "obtained_at": time.time(),
        })

        with patch("webbrowser.open", await self._drive_callback(client)):
            data = await client._authorize()

        assert data["access_token"] == "final"
        # Token persisted to disk, with the issuer recorded for later refreshes.
        assert client.get_access_token() == "final"
        assert client._load_token()["issuer"] == "https://as.example.com"

    @pytest.mark.asyncio
    async def test_authorize_registers_the_redirect_uri_it_will_use(self, tmp_path):
        """The port is chosen before registration, so the URI registered is the
        URI the browser is actually sent to."""
        client = self._make(tmp_path)
        self._stub_auth_server(
            client, registration_endpoint="https://as.example.com/register",
        )
        client._register_client = AsyncMock(return_value=("dyn-client", ""))
        client._exchange_code = AsyncMock(return_value={"access_token": "t"})

        with patch("webbrowser.open", await self._drive_callback(client, "code")):
            await client._authorize()

        client._register_client.assert_awaited_once()
        registered_uri = client._register_client.call_args.args[1]
        exchanged_uri = client._exchange_code.call_args.args[5]
        assert registered_uri == exchanged_uri
        assert ":" in registered_uri.split("//")[1].split("/")[0]  # carries a port
        assert client._exchange_code.call_args.args[2] == "dyn-client"

    @pytest.mark.asyncio
    async def test_authorize_timeout(self, tmp_path):
        client = self._make(tmp_path)
        self._stub_auth_server(client)

        with patch("webbrowser.open", return_value=True), \
             patch("asyncio.wait_for", AsyncMock(side_effect=asyncio.TimeoutError())):
            with pytest.raises(MCPOAuthError, match="timed out"):
                await client._authorize()

    @pytest.mark.asyncio
    async def test_authorize_url_is_properly_encoded(self, tmp_path):
        """Built with urlencode rather than string concatenation, so an endpoint
        that already carries a query string still produces a valid URL."""
        client = self._make(tmp_path)
        self._stub_auth_server(
            client, authorization_endpoint="https://as.example.com/auth?tenant=acme",
        )
        client._exchange_code = AsyncMock(return_value={"access_token": "t"})
        seen: list[str] = []

        drive = await self._drive_callback(client, "code")

        def capture(url):
            seen.append(url)
            return drive(url)

        with patch("webbrowser.open", capture):
            await client._authorize()

        assert "?tenant=acme&" in seen[0]
        assert "code_challenge_method=S256" in seen[0]
        assert "resource=https%3A%2F%2Fmcp.example.com" in seen[0]


class TestOAuthCallbackServer:
    """Drive the real callback HTTP handler via a started server.

    The port is bound once and the listening socket handed straight to
    `start_server`. The previous helper probed for a free port, closed it, and
    rebound later, leaving a window for another process to take the port.
    """

    def _make(self, tmp_path):
        return MCPOAuthClient("test_server", "https://mcp.example.com/", tmp_path)

    async def _serve(self, client, state, future):
        sock = client._bind_callback_socket()
        port = sock.getsockname()[1]
        server = await asyncio.start_server(
            client._make_callback_handler(state, future), sock=sock,
        )
        return server, port

    async def _get(self, port, path):
        reader, writer = await asyncio.open_connection("127.0.0.1", port)
        writer.write(f"GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n".encode())
        await writer.drain()
        body = await reader.read(4096)
        writer.close()
        return body

    @pytest.mark.asyncio
    async def test_callback_resolves_code_on_state_match(self, tmp_path):
        client = self._make(tmp_path)
        future: asyncio.Future = asyncio.get_running_loop().create_future()
        server, port = await self._serve(client, "expected-state", future)
        try:
            await self._get(port, "/callback?state=expected-state&code=the-code")
            assert await asyncio.wait_for(future, timeout=2) == "the-code"
        finally:
            server.close()
            await server.wait_closed()

    @pytest.mark.asyncio
    async def test_callback_rejects_state_mismatch(self, tmp_path):
        """A state mismatch must *resolve* the future with an error.

        Leaving it pending meant `_authorize` waited the full 300 seconds and
        then reported a timeout, hiding the real cause entirely.
        """
        client = self._make(tmp_path)
        future: asyncio.Future = asyncio.get_running_loop().create_future()
        server, port = await self._serve(client, "expected-state", future)
        try:
            body = await self._get(port, "/callback?state=wrong&code=x")
            assert b"State mismatch" in body
            assert future.done()
            with pytest.raises(MCPOAuthError, match="state"):
                future.result()
        finally:
            server.close()
            await server.wait_closed()

    @pytest.mark.asyncio
    async def test_callback_reports_authorization_server_error(self, tmp_path):
        client = self._make(tmp_path)
        future: asyncio.Future = asyncio.get_running_loop().create_future()
        server, port = await self._serve(client, "st", future)
        try:
            await self._get(
                port, "/callback?error=access_denied&error_description=User+said+no",
            )
            assert future.done()
            with pytest.raises(MCPOAuthError, match="User said no"):
                future.result()
        finally:
            server.close()
            await server.wait_closed()

    @pytest.mark.asyncio
    async def test_callback_handles_long_header_block(self, tmp_path):
        """The handler reads to the end of the headers rather than taking a
        single 4096-byte read, which truncated the request line for any client
        sending a large header block."""
        client = self._make(tmp_path)
        future: asyncio.Future = asyncio.get_running_loop().create_future()
        server, port = await self._serve(client, "st", future)
        try:
            reader, writer = await asyncio.open_connection("127.0.0.1", port)
            padding = "x" * 6000
            writer.write(
                f"GET /callback?state=st&code=c HTTP/1.1\r\n"
                f"Host: localhost\r\nX-Pad: {padding}\r\n\r\n".encode()
            )
            await writer.drain()
            await reader.read(4096)
            writer.close()
            assert await asyncio.wait_for(future, timeout=2) == "c"
        finally:
            server.close()
            await server.wait_closed()
