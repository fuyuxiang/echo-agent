"""MCP connection lifecycle: teardown, tool withdrawal, reconnection, leaks.

The manager's failure paths had no coverage, and that is where the defects were:

* ``stop_all`` cleared its own bookkeeping but never called ``registry.unregister``,
  so after shutdown the model was still offered every MCP tool and every call
  was guaranteed to fail. ``_registered_tools`` was written and never read.
* A connect attempt that failed after the transport was created left it open —
  for stdio, an orphaned child process — and on final failure the client never
  entered ``_clients``, so ``stop_all`` could never reach it either.
* Runtime reconnection did not exist, despite the docs promising exponential
  backoff. Reconnect logic lived only inside the *initial* connect loop, so a
  server that died stayed dead until the process restarted.
* ``notifications/tools/list_changed`` was queued and never consumed.
"""

from __future__ import annotations

import asyncio
from pathlib import Path
from unittest.mock import AsyncMock, MagicMock, patch

import pytest

from echo_agent.agent.tools.registry import ToolRegistry
from echo_agent.config.schema import MCPServerConfig
from echo_agent.mcp.manager import MCPManager


def _client(connected: bool = True, tools: list[dict] | None = None) -> MagicMock:
    client = MagicMock()
    client.is_connected = connected
    client.protocol_version = "2025-06-18"
    client.disconnect = AsyncMock()
    client.list_tools = AsyncMock(return_value=tools or [])
    client.call_tool = AsyncMock(return_value={"content": []})
    client.set_notification_handler = MagicMock()
    return client


def _manager(tmp_path: Path) -> MCPManager:
    return MCPManager(workspace=tmp_path, security_policy="block")


def _server_tools(registry: ToolRegistry) -> list[str]:
    """Per-server tool names only.

    ``mcp_resources`` / ``mcp_prompts`` are registered once for the manager as a
    whole (they take a ``server`` parameter), so they are not part of what these
    tests are about — per-server registration and withdrawal.
    """
    manager_wide = {"mcp_resources", "mcp_prompts"}
    return sorted(n for n in registry.tool_names if n not in manager_wide)


class TestToolWithdrawal:
    @pytest.mark.asyncio
    async def test_stop_all_unregisters_tools(self, tmp_path):
        """After shutdown the registry must not still advertise MCP tools."""
        manager = _manager(tmp_path)
        registry = ToolRegistry()
        client = _client(tools=[{"name": "search", "description": "Find things."}])
        manager._clients["srv"] = client
        manager._configs["srv"] = MCPServerConfig(command="echo")

        assert await manager.discover_tools(registry) == 1
        assert _server_tools(registry) == ["mcp_srv_search"]

        await manager.stop_all()

        # Nothing left at all: the manager-wide resource/prompt tools go too.
        assert registry.tool_names == []
        client.disconnect.assert_awaited_once()

    @pytest.mark.asyncio
    async def test_lost_connection_withdraws_only_that_servers_tools(self, tmp_path):
        manager = _manager(tmp_path)
        registry = ToolRegistry()
        for name in ("alpha", "beta"):
            manager._clients[name] = _client(tools=[{"name": "t", "description": "d"}])
            manager._configs[name] = MCPServerConfig(command="echo")

        await manager.discover_tools(registry)
        assert _server_tools(registry) == ["mcp_alpha_t", "mcp_beta_t"]

        await manager._teardown_server("alpha")

        assert _server_tools(registry) == ["mcp_beta_t"]

    @pytest.mark.asyncio
    async def test_disconnected_tools_are_not_advertised_to_the_model(self, tmp_path):
        """is_ready() reflects the live connection, so the registry stops
        offering a tool whose server has dropped rather than offering it and
        failing on use."""
        manager = _manager(tmp_path)
        registry = ToolRegistry()
        client = _client(tools=[{"name": "t", "description": "d"}])
        manager._clients["srv"] = client
        manager._configs["srv"] = MCPServerConfig(command="echo")
        await manager.discover_tools(registry)

        assert "mcp_srv_t" in registry.ready_tool_names

        client.is_connected = False
        assert "mcp_srv_t" not in registry.ready_tool_names
        # Still registered, but reported as not ready with a reason.
        ready, reason = dict(
            (name, (ready, why)) for name, ready, why in registry.get_readiness_report()
        )["mcp_srv_t"]
        assert ready is False
        assert "disconnected" in reason


class TestConnectFailureCleanup:
    @pytest.mark.asyncio
    async def test_failed_connect_closes_every_transport_it_created(self, tmp_path):
        """One transport per attempt, each closed before the next — the old loop
        reused a single transport and closed none, orphaning a child process on
        every retry."""
        manager = _manager(tmp_path)
        created: list[MagicMock] = []

        async def make_transport(name, cfg):
            transport = MagicMock()
            transport.close = AsyncMock()
            created.append(transport)
            return transport

        manager._create_transport = make_transport

        with patch("echo_agent.mcp.manager.MCPClient") as client_cls, \
             patch("asyncio.sleep", new=AsyncMock()):
            client_cls.return_value.connect = AsyncMock(side_effect=ConnectionError("nope"))
            client_cls.return_value.disconnect = AsyncMock()
            with pytest.raises(ConnectionError, match="after 5 attempts"):
                await manager._connect_server("srv", MCPServerConfig(command="echo"))

        assert len(created) == 5, "expected a fresh transport per attempt"
        assert client_cls.return_value.disconnect.await_count == 5
        assert "srv" not in manager._clients

    @pytest.mark.asyncio
    async def test_retry_then_succeed_registers_the_client(self, tmp_path):
        manager = _manager(tmp_path)
        manager._create_transport = AsyncMock(return_value=MagicMock(close=AsyncMock()))
        attempts = {"n": 0}

        async def connect(timeout):
            attempts["n"] += 1
            if attempts["n"] < 3:
                raise ConnectionError("not yet")

        with patch("echo_agent.mcp.manager.MCPClient") as client_cls, \
             patch("asyncio.sleep", new=AsyncMock()):
            client_cls.return_value.connect = connect
            client_cls.return_value.disconnect = AsyncMock()
            await manager._connect_server("srv", MCPServerConfig(command="echo"))

        assert attempts["n"] == 3
        assert "srv" in manager._clients
        # A supervisor now watches the established connection.
        assert "srv" in manager._supervisors
        await manager.stop_all()

    @pytest.mark.asyncio
    async def test_stop_during_retry_abandons_the_attempt(self, tmp_path):
        manager = _manager(tmp_path)
        manager._stopping = True
        manager._create_transport = AsyncMock()

        await manager._connect_server("srv", MCPServerConfig(command="echo"))

        manager._create_transport.assert_not_called()


class TestReconnection:
    @pytest.mark.asyncio
    async def test_supervisor_rebuilds_a_dropped_connection(self, tmp_path):
        """The behaviour the documentation already promised and the code lacked."""
        manager = _manager(tmp_path)
        registry = ToolRegistry()
        manager._registry = registry
        manager._configs["srv"] = MCPServerConfig(command="echo")

        dead = _client(connected=False)
        manager._clients["srv"] = dead
        manager._registered_tools["srv"] = ["mcp_srv_stale"]
        registry.register(_stale_tool("mcp_srv_stale"))

        fresh = _client(tools=[{"name": "renewed", "description": "d"}])

        async def reconnect(name, cfg):
            manager._clients[name] = fresh

        manager._connect_server = reconnect

        # One health-check cycle. It reports True — supervision continues past a
        # successful rebuild, which is what keeps a later disconnect observable.
        assert await manager._supervise_once("srv") is True

        assert registry.tool_names == ["mcp_srv_renewed"]
        dead.disconnect.assert_awaited_once()

    @pytest.mark.asyncio
    async def test_second_disconnect_is_also_rebuilt(self, tmp_path):
        """Reconnection must not be one-shot.

        The supervisor used to return after a successful rebuild, on the theory
        that the new connection brought its own supervisor. It did not:
        ``_connect_server`` reaches ``_start_supervisor`` while this task is
        still alive, and that call declines to create a second watcher. So the
        rebuilt connection went unwatched and the next drop was never noticed —
        tools stayed advertised pointing at a dead server.
        """
        manager = _manager(tmp_path)
        registry = ToolRegistry()
        manager._registry = registry
        manager._configs["srv"] = MCPServerConfig(command="echo")

        dead = _client(connected=False)
        manager._clients["srv"] = dead
        manager._registered_tools["srv"] = ["mcp_srv_stale"]
        registry.register(_stale_tool("mcp_srv_stale"))

        generations: list[MagicMock] = []

        async def reconnect(name, cfg):
            fresh = _client(tools=[{"name": f"gen{len(generations) + 1}", "description": "d"}])
            generations.append(fresh)
            manager._clients[name] = fresh
            # What the real _connect_server does at the end, and the exact call
            # that used to be a no-op while leaving the old supervisor exiting.
            manager._start_supervisor(name)

        manager._connect_server = reconnect

        async def tick(_delay):
            # Drive the loop: drop the live connection until two rebuilds have
            # happened, then let the supervisor wind down.
            if len(generations) >= 2:
                manager._stopping = True
                return
            if generations:
                generations[-1].is_connected = False

        with patch("asyncio.sleep", new=tick):
            task = asyncio.create_task(manager._supervise("srv"))
            manager._supervisors["srv"] = task
            await task

        assert len(generations) == 2, "the second disconnect must be rebuilt too"
        # Both dead connections were torn down, not just the first.
        dead.disconnect.assert_awaited_once()
        generations[0].disconnect.assert_awaited_once()
        # And the registry tracks the *current* generation only.
        assert _server_tools(registry) == ["mcp_srv_gen2"]
        assert manager._supervisors["srv"] is task, "no second supervisor was spawned"

    @pytest.mark.asyncio
    async def test_start_supervisor_does_not_duplicate_a_live_watcher(self, tmp_path):
        manager = _manager(tmp_path)
        manager._clients["srv"] = _client()
        manager._configs["srv"] = MCPServerConfig(command="echo")

        manager._start_supervisor("srv")
        first = manager._supervisors["srv"]
        manager._start_supervisor("srv")
        assert manager._supervisors["srv"] is first

        manager._stopping = True
        first.cancel()
        await asyncio.gather(first, return_exceptions=True)

    @pytest.mark.asyncio
    async def test_supervisor_gives_up_cleanly_when_reconnect_fails(self, tmp_path):
        manager = _manager(tmp_path)
        registry = ToolRegistry()
        manager._registry = registry
        manager._configs["srv"] = MCPServerConfig(command="echo")
        manager._clients["srv"] = _client(connected=False)
        manager._registered_tools["srv"] = ["mcp_srv_stale"]
        registry.register(_stale_tool("mcp_srv_stale"))

        manager._connect_server = AsyncMock(side_effect=ConnectionError("still down"))

        with patch("asyncio.sleep", new=AsyncMock()):
            await manager._supervise("srv")

        # Tools stay withdrawn rather than being left pointing at a dead server.
        assert registry.tool_names == []

    @pytest.mark.asyncio
    async def test_list_changed_notification_triggers_rediscovery(self, tmp_path):
        """The notification was queued with no consumer, so a server adding or
        removing a tool had no way to inform us."""
        manager = _manager(tmp_path)
        registry = ToolRegistry()
        manager._registry = registry
        manager._configs["srv"] = MCPServerConfig(command="echo")
        client = _client(tools=[{"name": "first", "description": "d"}])
        manager._clients["srv"] = client

        await manager.discover_tools(registry)
        assert _server_tools(registry) == ["mcp_srv_first"]

        client.list_tools = AsyncMock(return_value=[{"name": "second", "description": "d"}])
        await manager._on_notification("srv", "notifications/tools/list_changed", {})

        assert _server_tools(registry) == ["mcp_srv_second"]

    @pytest.mark.asyncio
    async def test_unrelated_notifications_are_ignored(self, tmp_path):
        manager = _manager(tmp_path)
        registry = ToolRegistry()
        manager._registry = registry
        manager._configs["srv"] = MCPServerConfig(command="echo")
        client = _client(tools=[{"name": "first", "description": "d"}])
        manager._clients["srv"] = client
        await manager.discover_tools(registry)

        await manager._on_notification("srv", "notifications/progress", {"p": 1})

        assert _server_tools(registry) == ["mcp_srv_first"]
        client.list_tools.assert_awaited_once()  # not re-discovered


class TestTransportSelection:
    @pytest.mark.asyncio
    async def test_both_url_and_command_is_refused(self, tmp_path):
        """Config validation catches this first; the manager refuses too rather
        than silently preferring one, which is how a typo went unnoticed."""
        manager = _manager(tmp_path)
        cfg = MCPServerConfig(command="echo")
        object.__setattr__(cfg, "url", "https://mcp.example.com/mcp")

        with pytest.raises(ValueError, match="exactly one"):
            await manager._create_transport("srv", cfg)

    @pytest.mark.asyncio
    async def test_disallowed_url_scheme_is_refused(self, tmp_path):
        """MCP URLs bypassed the project's shared outbound-network gate entirely,
        accepting any scheme including file://."""
        manager = _manager(tmp_path)
        cfg = MCPServerConfig(url="https://mcp.example.com/mcp")
        object.__setattr__(cfg, "url", "file:///etc/passwd")

        with pytest.raises(ValueError, match="scheme"):
            await manager._create_transport("srv", cfg)

    @pytest.mark.asyncio
    async def test_http_transport_gets_the_call_timeout(self, tmp_path):
        """cfg.timeout bounds a tool call; it used to be overridden by the
        connect budget imposed as an aiohttp `total`."""
        manager = _manager(tmp_path)
        cfg = MCPServerConfig(url="https://mcp.example.com/mcp", timeout=90)

        with patch("echo_agent.mcp.manager.check_url_ssrf", create=True):
            with patch("echo_agent.security.net_guard.check_url_ssrf",
                       new=AsyncMock(return_value=None)):
                transport = await manager._create_transport("srv", cfg)

        assert transport._call_timeout == 90.0

    @pytest.mark.asyncio
    async def test_private_address_only_warns(self, tmp_path):
        """Pointing MCP at a LAN or dev-box server is a normal deployment, unlike
        a model-supplied fetch target."""
        manager = _manager(tmp_path)
        cfg = MCPServerConfig(url="http://127.0.0.1:8080/mcp")

        with patch("echo_agent.security.net_guard.check_url_ssrf",
                   new=AsyncMock(return_value="Blocked: loopback")):
            transport = await manager._create_transport("srv", cfg)

        assert transport is not None


class TestHealthReport:
    @pytest.mark.asyncio
    async def test_report_reflects_live_state_not_config(self, tmp_path):
        """"MCP is healthy" must not be able to mean merely "MCP is configured"."""
        manager = _manager(tmp_path)
        registry = ToolRegistry()
        manager._configs = {
            "up": MCPServerConfig(command="echo"),
            "down": MCPServerConfig(url="https://mcp.example.com/mcp", trust_level="trusted"),
        }
        manager._clients["up"] = _client(tools=[{"name": "t", "description": "d"}])
        await manager.discover_tools(registry)

        report = manager.health_report()

        assert report["up"] == {
            "connected": True, "transport": "stdio", "trust_level": "untrusted",
            "tools": 1, "protocol_version": "2025-06-18",
        }
        assert report["down"]["connected"] is False
        assert report["down"]["transport"] == "http"
        assert report["down"]["trust_level"] == "trusted"
        assert report["down"]["tools"] == 0


def _stale_tool(name: str):
    """A minimal Tool standing in for a previously registered MCP adapter."""
    from echo_agent.tools import Tool, ToolResult

    class _Stale(Tool):
        def __init__(self) -> None:
            self.name = name
            self.description = "stale"
            self.parameters = {"type": "object", "properties": {}}

        async def execute(self, params, ctx=None):  # pragma: no cover - never called
            return ToolResult(success=False, error="stale")

    return _Stale()


class TestCredentialRecovery:
    """Recovery from a rejected credential.

    A token can be revoked or invalidated long before its nominal expiry. The
    401 used to surface as a generic connection error: the stored token stayed on
    disk, `ensure_token()` saw it as unexpired, and every reconnect replayed the
    same dead credential until someone deleted the file by hand.
    """

    @pytest.mark.asyncio
    async def test_401_marks_the_transport_and_takes_it_down(self):
        from echo_agent.mcp.transport import MCPUnauthorizedError, StreamableHttpTransport

        transport = StreamableHttpTransport(url="https://x/mcp")
        transport._connected = True
        resp = MagicMock()
        resp.status = 401
        resp.headers = {
            "WWW-Authenticate":
                'Bearer resource_metadata="https://x/.well-known/oauth-protected-resource"'
        }
        resp.text = AsyncMock(return_value="token revoked")

        with pytest.raises(MCPUnauthorizedError) as excinfo:
            await transport._check_status(resp, {"method": "tools/call"})

        assert "resource_metadata" in excinfo.value.www_authenticate
        assert transport.auth_failed is True
        # Must report itself down, or the supervisor never rebuilds.
        assert transport.is_connected is False

    @pytest.mark.asyncio
    async def test_expired_session_takes_the_transport_down(self):
        from echo_agent.mcp.transport import StreamableHttpTransport

        transport = StreamableHttpTransport(url="https://x/mcp")
        transport._connected = True
        transport._session_id = "sess-1"
        resp = MagicMock()
        resp.status = 404
        resp.headers = {}
        resp.text = AsyncMock(return_value="unknown session")

        with pytest.raises(ConnectionError, match="session expired"):
            await transport._check_status(resp, {"method": "tools/list"})

        assert transport.session_expired is True
        assert transport.is_connected is False

    @pytest.mark.asyncio
    async def test_supervisor_refreshes_the_token_before_rebuilding(self, tmp_path):
        manager = _manager(tmp_path)
        manager._configs["srv"] = MCPServerConfig(url="https://x/mcp", auth="oauth")

        transport = MagicMock()
        transport.auth_failed = True
        client = _client(connected=False)
        client._transport = transport
        manager._clients["srv"] = client

        manager._try_reauthorize = AsyncMock(return_value=True)
        manager._connect_server = AsyncMock(side_effect=ConnectionError("stop after refresh"))

        with patch("asyncio.sleep", new=AsyncMock()):
            await manager._supervise("srv")

        manager._try_reauthorize.assert_awaited_once_with("srv")

    @pytest.mark.asyncio
    async def test_plain_disconnect_does_not_touch_credentials(self, tmp_path):
        """An ordinary dropped connection must not trigger a token refresh."""
        manager = _manager(tmp_path)
        manager._configs["srv"] = MCPServerConfig(url="https://x/mcp", auth="oauth")

        transport = MagicMock()
        transport.auth_failed = False
        client = _client(connected=False)
        client._transport = transport
        manager._clients["srv"] = client

        manager._try_reauthorize = AsyncMock(return_value=True)
        manager._connect_server = AsyncMock(side_effect=ConnectionError("down"))

        with patch("asyncio.sleep", new=AsyncMock()):
            await manager._supervise("srv")

        manager._try_reauthorize.assert_not_awaited()

    @pytest.mark.asyncio
    async def test_reauthorize_is_a_noop_for_non_oauth_servers(self, tmp_path):
        manager = _manager(tmp_path)
        manager._configs["stdio"] = MCPServerConfig(command="echo")
        assert await manager._try_reauthorize("stdio") is False
        manager._configs["plain"] = MCPServerConfig(url="https://x/mcp")
        assert await manager._try_reauthorize("plain") is False

    @pytest.mark.asyncio
    async def test_reauthorize_reports_when_refresh_is_impossible(self, tmp_path):
        """No refresh token means only interactive consent would help, which a
        background supervisor must not launch on its own."""
        manager = _manager(tmp_path)
        manager._configs["srv"] = MCPServerConfig(url="https://x/mcp", auth="oauth")

        oauth = MagicMock()
        oauth.refresh_after_401 = AsyncMock(return_value=None)
        with patch("echo_agent.mcp.oauth.MCPOAuthClient", return_value=oauth):
            assert await manager._try_reauthorize("srv") is False

    @pytest.mark.asyncio
    async def test_reauthorize_succeeds_when_refresh_returns_a_token(self, tmp_path):
        manager = _manager(tmp_path)
        manager._configs["srv"] = MCPServerConfig(url="https://x/mcp", auth="oauth")

        oauth = MagicMock()
        oauth.refresh_after_401 = AsyncMock(return_value="new-token")
        with patch("echo_agent.mcp.oauth.MCPOAuthClient", return_value=oauth):
            assert await manager._try_reauthorize("srv") is True
