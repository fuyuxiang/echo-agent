"""Stored OAuth tokens must never cross configured MCP resource boundaries."""

import json
import time
from unittest.mock import AsyncMock

import pytest

from echo_agent.config.schema import MCPServerConfig
from echo_agent.mcp.manager import MCPManager
from echo_agent.mcp.oauth import MCPOAuthClient, MCPOAuthError


@pytest.mark.asyncio
@pytest.mark.parametrize("replacement", ["https://other.example/mcp", "https://original.example/different-resource"])
async def test_changed_resource_does_not_forward_old_bearer(tmp_path, monkeypatch, replacement):
    token_dir = tmp_path / "data/mcp_tokens"
    original = MCPOAuthClient("srv", "https://original.example/mcp", token_dir)
    original._save_token({
        "access_token": "old-resource-only-secret", "refresh_token": "old-refresh-secret",
        "obtained_at": time.time(), "expires_in": 3600,
        "issuer": "https://issuer.example", "token_endpoint": "https://issuer.example/token",
    })
    reauthorize = AsyncMock(return_value={"access_token": "new-resource-token"})
    monkeypatch.setattr(MCPOAuthClient, "_authorize", reauthorize)
    manager = MCPManager(tmp_path)
    monkeypatch.setattr(manager, "_check_url_policy", AsyncMock())
    transport = await manager._create_transport("srv", MCPServerConfig(url=replacement, auth="oauth"))
    try:
        assert transport._headers["Authorization"] == "Bearer new-resource-token"
        reauthorize.assert_awaited_once()
        # Rejecting the cached token does not destroy the operator's old file.
        assert "old-resource-only-secret" in original._token_file.read_text()
    finally:
        await transport.close()


def test_unbound_legacy_token_is_not_reused(tmp_path):
    client = MCPOAuthClient("srv", "https://original.example/mcp", tmp_path)
    client._token_file.write_text(json.dumps({
        "access_token": "legacy-secret", "obtained_at": time.time(), "expires_in": 3600,
    }))
    assert client.get_access_token() is None
    assert client._token_file.exists()


@pytest.mark.asyncio
async def test_noninteractive_authorization_fails_before_network_or_callback(tmp_path, monkeypatch):
    client = MCPOAuthClient("srv", "https://original.example/mcp", tmp_path)
    monkeypatch.setattr("sys.stdin.isatty", lambda: False)
    discovery = AsyncMock()
    monkeypatch.setattr(client, "_resolve_auth_server", discovery)
    with pytest.raises(MCPOAuthError, match="no interactive terminal"):
        await client.ensure_token()
    discovery.assert_not_awaited()
