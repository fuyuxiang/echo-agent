"""tools.mcp.enabled is a real switch at runtime, not just in the wizard.

The switch was written by the setup wizard, annotated ``"status": "effective"``
in the schema, and read by nothing. Un-checking MCP wrote ``enabled: false`` and
every configured server connected anyway on the next start — the same "fake
toggle" the wizard code comments claim to have fixed, reappearing one layer down.

Nothing covered ``_start_mcp`` at all, which is why. These tests call the real
method with the surrounding loop stubbed, so the switch cannot silently stop
being read again.
"""

from __future__ import annotations

from pathlib import Path
from unittest.mock import AsyncMock, MagicMock, patch

import pytest

from echo_agent.agent.loop import AgentLoop
from echo_agent.config.schema import Config, MCPServerConfig


def _loop(config: Config, tmp_path: Path) -> AgentLoop:
    """An object that is an AgentLoop for the purposes of _start_mcp.

    AgentLoop.__init__ builds a provider, session store, memory and a scheduler,
    none of which _start_mcp touches. Constructing the attributes it does read is
    both faster and a tighter statement of what this behaviour depends on.
    """
    loop = object.__new__(AgentLoop)
    loop.config = config
    loop.workspace = tmp_path
    loop.tools = MagicMock()
    loop.tools.tool_names = []
    loop.mcp_manager = None
    return loop


def _config(*, enabled: bool, servers: dict[str, MCPServerConfig]) -> Config:
    config = Config()
    config.tools.mcp.enabled = enabled
    config.tools.mcp_servers = servers
    return config


@pytest.mark.asyncio
async def test_disabled_switch_starts_no_manager(tmp_path):
    """The regression: servers configured AND the switch off must connect nothing."""
    config = _config(enabled=False, servers={"srv": MCPServerConfig(command="echo")})
    loop = _loop(config, tmp_path)

    with patch("echo_agent.mcp.manager.MCPManager") as manager_cls:
        await loop._start_mcp()

    manager_cls.assert_not_called()
    assert loop.mcp_manager is None


@pytest.mark.asyncio
async def test_enabled_switch_starts_and_discovers(tmp_path):
    config = _config(enabled=True, servers={"srv": MCPServerConfig(command="echo")})
    loop = _loop(config, tmp_path)

    manager = MagicMock()
    manager.start_all = AsyncMock()
    manager.discover_tools = AsyncMock(return_value=1)

    with patch("echo_agent.mcp.manager.MCPManager", return_value=manager):
        await loop._start_mcp()

    manager.start_all.assert_awaited_once()
    manager.discover_tools.assert_awaited_once_with(loop.tools)
    assert loop.mcp_manager is manager


@pytest.mark.asyncio
async def test_enabled_switch_without_servers_is_a_noop(tmp_path):
    config = _config(enabled=True, servers={})
    loop = _loop(config, tmp_path)

    with patch("echo_agent.mcp.manager.MCPManager") as manager_cls:
        await loop._start_mcp()

    manager_cls.assert_not_called()


@pytest.mark.asyncio
async def test_security_policy_is_passed_through(tmp_path):
    """mcpSecurityPolicy governs whether a suspicious tool is dropped, so it has
    to actually reach the manager."""
    config = _config(enabled=True, servers={"srv": MCPServerConfig(command="echo")})
    config.tools.mcp_security_policy = "warn"
    loop = _loop(config, tmp_path)

    manager = MagicMock()
    manager.start_all = AsyncMock()
    manager.discover_tools = AsyncMock(return_value=0)

    with patch("echo_agent.mcp.manager.MCPManager", return_value=manager) as cls:
        await loop._start_mcp()

    assert cls.call_args.kwargs["security_policy"] == "warn"


@pytest.mark.asyncio
async def test_startup_failure_does_not_take_down_the_agent(tmp_path):
    """MCP is an optional integration: a server that cannot be reached must not
    prevent the agent from starting."""
    config = _config(enabled=True, servers={"srv": MCPServerConfig(command="echo")})
    loop = _loop(config, tmp_path)

    manager = MagicMock()
    manager.start_all = AsyncMock(side_effect=ConnectionError("unreachable"))

    with patch("echo_agent.mcp.manager.MCPManager", return_value=manager):
        await loop._start_mcp_background()  # swallows and logs


class TestServerFiltering:
    """_filter_mcp_servers applies deployment-level policy before any connect."""

    def test_http_servers_skipped_when_network_denied(self, tmp_path):
        """networkPolicy=deny is the packaged default, so an HTTP MCP server is
        skipped out of the box until the operator opens outbound access."""
        config = _config(enabled=True, servers={
            "remote": MCPServerConfig(url="https://mcp.example.com/mcp"),
            "local": MCPServerConfig(command="echo"),
        })
        assert config.execution.network_policy == "deny"
        loop = _loop(config, tmp_path)

        assert set(loop._filter_mcp_servers(config.tools.mcp_servers)) == {"local"}

    def test_http_servers_kept_when_network_allowed(self, tmp_path):
        config = _config(enabled=True, servers={
            "remote": MCPServerConfig(url="https://mcp.example.com/mcp"),
        })
        config.execution.network_policy = "allow"
        loop = _loop(config, tmp_path)

        assert set(loop._filter_mcp_servers(config.tools.mcp_servers)) == {"remote"}

    def test_stdio_servers_skipped_on_public_gateway_without_elevation(self, tmp_path):
        """A stdio server is a child process on the host — not something a public
        gateway deployment should spawn by default. Outbound access is opened here
        so the HTTP server isolates the profile rule from the network rule.
        """
        config = _config(enabled=True, servers={
            "local": MCPServerConfig(command="echo"),
            "remote": MCPServerConfig(url="https://mcp.example.com/mcp"),
        })
        config.execution.network_policy = "allow"
        config.security.profile = "public_gateway"
        config.permissions.elevated.enabled = False
        loop = _loop(config, tmp_path)

        assert set(loop._filter_mcp_servers(config.tools.mcp_servers)) == {"remote"}
