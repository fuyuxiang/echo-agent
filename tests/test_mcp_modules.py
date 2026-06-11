"""Tests for MCP modules: tool_adapter, manager, oauth."""
from __future__ import annotations

import json
import os
import time
from pathlib import Path
from unittest.mock import AsyncMock, MagicMock, patch

import pytest


# ══════════════════════════════════════════════════════════════════════════════
# 1. MCPToolAdapter
# ══════════════════════════════════════════════════════════════════════════════


class TestMCPToolAdapterSanitizeName:
    def test_simple(self):
        from echo_agent.mcp.tool_adapter import _sanitize_name
        assert _sanitize_name("myserver", "my_tool") == "mcp_myserver_my_tool"

    def test_special_chars(self):
        from echo_agent.mcp.tool_adapter import _sanitize_name
        result = _sanitize_name("my-server", "tool.v2")
        assert result == "mcp_my_server_tool_v2"

    def test_spaces_and_symbols(self):
        from echo_agent.mcp.tool_adapter import _sanitize_name
        result = _sanitize_name("srv@1", "fn #2")
        assert result == "mcp_srv_1_fn__2"


class TestMCPToolAdapterConvertSchema:
    def test_with_input_schema(self):
        from echo_agent.mcp.tool_adapter import _convert_mcp_schema
        mcp_tool = {"inputSchema": {"type": "object", "properties": {"x": {"type": "string"}}}}
        result = _convert_mcp_schema(mcp_tool)
        assert result["type"] == "object"
        assert "x" in result["properties"]

    def test_empty_schema(self):
        from echo_agent.mcp.tool_adapter import _convert_mcp_schema
        result = _convert_mcp_schema({})
        assert result == {"type": "object", "properties": {}}

    def test_none_schema(self):
        from echo_agent.mcp.tool_adapter import _convert_mcp_schema
        result = _convert_mcp_schema({"inputSchema": None})
        assert result == {"type": "object", "properties": {}}


class TestMCPToolAdapterExecute:
    def _make_adapter(self):
        from echo_agent.mcp.tool_adapter import MCPToolAdapter

        client = MagicMock()
        client.call_tool = AsyncMock()
        mcp_tool = {
            "name": "search",
            "description": "Search things",
            "inputSchema": {"type": "object", "properties": {"q": {"type": "string"}}},
        }
        adapter = MCPToolAdapter(server_name="test_srv", mcp_tool=mcp_tool, client=client)
        return adapter, client

    @pytest.mark.asyncio
    async def test_execute_success(self):
        adapter, client = self._make_adapter()
        client.call_tool.return_value = {
            "content": [{"type": "text", "text": "Found 3 results"}],
            "isError": False,
        }
        result = await adapter.execute({"q": "hello"})
        assert result.success is True
        assert "Found 3 results" in result.output
        assert result.metadata["mcp_server"] == "test_srv"
        assert result.metadata["mcp_tool"] == "search"

    @pytest.mark.asyncio
    async def test_execute_error_response(self):
        adapter, client = self._make_adapter()
        client.call_tool.return_value = {
            "content": [{"type": "text", "text": "Not found"}],
            "isError": True,
        }
        result = await adapter.execute({"q": "missing"})
        assert result.success is False
        assert "Not found" in result.error
        assert result.output == ""

    @pytest.mark.asyncio
    async def test_execute_timeout(self):
        adapter, client = self._make_adapter()
        client.call_tool.side_effect = TimeoutError("timed out after 120s")
        result = await adapter.execute({"q": "slow"})
        assert result.success is False
        assert "timed out" in result.error

    @pytest.mark.asyncio
    async def test_execute_exception(self):
        adapter, client = self._make_adapter()
        client.call_tool.side_effect = ConnectionError("connection lost")
        result = await adapter.execute({"q": "x"})
        assert result.success is False
        assert "connection lost" in result.error

    @pytest.mark.asyncio
    async def test_execute_mixed_content(self):
        adapter, client = self._make_adapter()
        client.call_tool.return_value = {
            "content": [
                {"type": "text", "text": "Part 1"},
                {"type": "image", "mimeType": "image/png"},
                {"type": "resource", "resource": {"uri": "file:///x", "text": "resource data"}},
                "raw string part",
            ],
            "isError": False,
        }
        result = await adapter.execute({"q": "multi"})
        assert result.success is True
        assert "Part 1" in result.output
        assert "[image: image/png]" in result.output
        assert "resource data" in result.output
        assert "raw string part" in result.output

    def test_execution_mode(self):
        adapter, _ = self._make_adapter()
        assert adapter.execution_mode({}) == "side_effect"

    def test_adapter_attributes(self):
        adapter, _ = self._make_adapter()
        assert adapter.name == "mcp_test_srv_search"
        assert adapter.description == "Search things"
        assert "q" in adapter.parameters.get("properties", {})


# ══════════════════════════════════════════════════════════════════════════════
# 2. MCPManager
# ══════════════════════════════════════════════════════════════════════════════


class TestMCPManager:
    def _make(self, tmp_path=None):
        from echo_agent.mcp.manager import MCPManager
        workspace = tmp_path or Path("/tmp/test_mcp_workspace")
        return MCPManager(workspace=workspace, security_policy="block")

    @pytest.mark.asyncio
    async def test_start_all_skips_disabled(self, tmp_path):
        from echo_agent.config.schema import MCPServerConfig

        mgr = self._make(tmp_path)
        disabled_cfg = MCPServerConfig(enabled=False, command="echo")
        enabled_cfg = MCPServerConfig(enabled=True, command="echo", args=["hello"])

        with patch.object(mgr, "_connect_server", new_callable=AsyncMock) as mock_connect:
            await mgr.start_all({"disabled_srv": disabled_cfg, "enabled_srv": enabled_cfg})
            mock_connect.assert_called_once()
            call_name = mock_connect.call_args[0][0]
            assert call_name == "enabled_srv"

    @pytest.mark.asyncio
    async def test_start_all_handles_connection_error(self, tmp_path):
        from echo_agent.config.schema import MCPServerConfig

        mgr = self._make(tmp_path)
        cfg = MCPServerConfig(enabled=True, command="bad_cmd")

        with patch.object(mgr, "_connect_server", new_callable=AsyncMock) as mock_connect:
            mock_connect.side_effect = ConnectionError("failed")
            # Should not raise, just log
            await mgr.start_all({"srv": cfg})

    @pytest.mark.asyncio
    async def test_discover_tools(self, tmp_path):

        mgr = self._make(tmp_path)
        mock_client = MagicMock()
        mock_client.is_connected = True

        mgr._clients["srv1"] = mock_client

        mock_registry = MagicMock()
        mock_registry.tool_names = ["builtin_tool"]

        with patch.object(mgr, "_register_server_tools", new_callable=AsyncMock) as mock_reg:
            mock_reg.return_value = 3
            total = await mgr.discover_tools(mock_registry)
            assert total == 3
            mock_reg.assert_called_once()

    @pytest.mark.asyncio
    async def test_discover_tools_skips_disconnected(self, tmp_path):
        mgr = self._make(tmp_path)
        mock_client = MagicMock()
        mock_client.is_connected = False
        mgr._clients["srv1"] = mock_client

        mock_registry = MagicMock()
        mock_registry.tool_names = []

        with patch.object(mgr, "_register_server_tools", new_callable=AsyncMock) as mock_reg:
            total = await mgr.discover_tools(mock_registry)
            assert total == 0
            mock_reg.assert_not_called()

    def test_resolve_env_vars(self, tmp_path):
        mgr = self._make(tmp_path)
        with patch.dict(os.environ, {"MY_TOKEN": "secret123", "HOST": "localhost"}):
            result = mgr._resolve_env_vars({
                "Authorization": "Bearer ${MY_TOKEN}",
                "X-Host": "${HOST}:8080",
                "Plain": "no_vars_here",
                "Missing": "${NOT_SET}",
            })
            assert result["Authorization"] == "Bearer secret123"
            assert result["X-Host"] == "localhost:8080"
            assert result["Plain"] == "no_vars_here"
            assert result["Missing"] == "${NOT_SET}"

    def test_resolve_env_vars_empty(self, tmp_path):
        mgr = self._make(tmp_path)
        assert mgr._resolve_env_vars({}) == {}

    def test_connected_servers(self, tmp_path):
        mgr = self._make(tmp_path)
        c1 = MagicMock()
        c1.is_connected = True
        c2 = MagicMock()
        c2.is_connected = False
        mgr._clients = {"a": c1, "b": c2}
        assert mgr.connected_servers == ["a"]


# ══════════════════════════════════════════════════════════════════════════════
# 3. MCPOAuthClient
# ══════════════════════════════════════════════════════════════════════════════


class TestMCPOAuthClient:
    def _make(self, tmp_path):
        from echo_agent.mcp.oauth import MCPOAuthClient
        return MCPOAuthClient(
            server_name="test_server",
            server_url="https://mcp.example.com",
            token_dir=tmp_path,
        )

    def test_get_access_token_no_file(self, tmp_path):
        client = self._make(tmp_path)
        assert client.get_access_token() is None

    def test_get_access_token_valid(self, tmp_path):
        client = self._make(tmp_path)
        token_data = {
            "access_token": "valid_token_123",
            "expires_in": 3600,
            "obtained_at": time.time(),
        }
        (tmp_path / "test_server.json").write_text(json.dumps(token_data))
        assert client.get_access_token() == "valid_token_123"

    def test_get_access_token_expired(self, tmp_path):
        client = self._make(tmp_path)
        token_data = {
            "access_token": "expired_token",
            "expires_in": 3600,
            "obtained_at": time.time() - 4000,  # expired
        }
        (tmp_path / "test_server.json").write_text(json.dumps(token_data))
        assert client.get_access_token() is None

    def test_is_expired_true(self, tmp_path):
        client = self._make(tmp_path)
        token_data = {
            "obtained_at": time.time() - 7200,
            "expires_in": 3600,
        }
        assert client._is_expired(token_data) is True

    def test_is_expired_false(self, tmp_path):
        client = self._make(tmp_path)
        token_data = {
            "obtained_at": time.time(),
            "expires_in": 3600,
        }
        assert client._is_expired(token_data) is False

    def test_is_expired_with_margin(self, tmp_path):
        """Token should be considered expired 60s before actual expiry."""
        client = self._make(tmp_path)
        token_data = {
            "obtained_at": time.time() - 3550,  # 50s before expiry
            "expires_in": 3600,
        }
        # time.time() > obtained_at + expires_in - 60 => now > now - 3550 + 3600 - 60 = now - 10
        # That means now > now - 10 => True => expired
        assert client._is_expired(token_data) is True

    def test_load_token_corrupt_file(self, tmp_path):
        client = self._make(tmp_path)
        (tmp_path / "test_server.json").write_text("not valid json{{{")
        assert client._load_token() is None

    def test_save_and_load_token(self, tmp_path):
        client = self._make(tmp_path)
        data = {"access_token": "tok", "expires_in": 3600, "obtained_at": time.time()}
        client._save_token(data)
        loaded = client._load_token()
        assert loaded["access_token"] == "tok"

    @pytest.mark.asyncio
    async def test_ensure_token_valid_cached(self, tmp_path):
        client = self._make(tmp_path)
        token_data = {
            "access_token": "cached_token",
            "expires_in": 3600,
            "obtained_at": time.time(),
        }
        (tmp_path / "test_server.json").write_text(json.dumps(token_data))
        result = await client.ensure_token()
        assert result == "cached_token"
