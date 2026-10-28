"""Operator-installed runtime extensions must never silently lose their safety boundary."""
from unittest.mock import MagicMock

import pytest

from echo_agent.config.schema import Config
from echo_agent.plugins.manager import PluginManager
from echo_agent.security.tool_policy import tool_policy_decision


# /***********************************************************
#  *                      本地方法                          *
#  ***********************************************************/

@pytest.mark.asyncio
@pytest.mark.parametrize("enabled", [True, False])
async def test_required_plugin_missing_or_disabled_blocks_startup(tmp_path, enabled):
    """A configured mandatory connector cannot disappear into ordinary assistant mode."""
    config = Config(plugins={"enabled": enabled, "required": ["missing-connector"]})
    manager = PluginManager(config=config, workspace=tmp_path, bus=MagicMock(), tool_registry=MagicMock())
    with pytest.raises(RuntimeError, match="required plugin"):
        await manager.discover_and_load()


def test_extension_allowlist_cannot_override_explicit_denial():
    """An installed extension narrows tools; explicit operator deny remains authoritative."""
    from echo_agent.plugins.runtime import RuntimeExtension

    extension = RuntimeExtension()
    extension.allowed_tool_patterns = ("mcp_catalog_*", "clarify")
    config = Config(tools={"profile": "full", "allow": ["exec"], "deny": ["clarify"]})
    config._runtime_extension = extension
    assert not tool_policy_decision(config, "exec")[0]
    assert not tool_policy_decision(config, "clarify")[0]
    assert tool_policy_decision(config, "mcp_catalog_list")[0]


@pytest.mark.asyncio
async def test_missing_required_plugin_releases_an_already_activated_extension(tmp_path, monkeypatch):
    """Bootstrap failure unwinds the optional plugin that activated before the failure."""
    from echo_agent.plugins.manifest import PluginManifest, PluginRecord
    from echo_agent.plugins.runtime import RuntimeExtension

    record = PluginRecord(manifest=PluginManifest(name="optional", permissions=["hook.register"]), source="entrypoint",
                          module={"activate": lambda ctx: ctx.register_runtime_extension(RuntimeExtension())})
    monkeypatch.setattr("echo_agent.plugins.manager.discover_all", lambda **kwargs: [record])
    config = Config(plugins={"required": ["optional", "missing-connector"]})
    manager = PluginManager(config=config, workspace=tmp_path, bus=MagicMock(), tool_registry=MagicMock())
    with pytest.raises(RuntimeError, match="required plugin"):
        await manager.discover_and_load()
    assert manager.runtime_extension is None
    assert not manager._contexts


@pytest.mark.asyncio
@pytest.mark.parametrize("permission_mode", ["strict", "compat"])
async def test_denied_runtime_registration_is_always_fail_closed(tmp_path, monkeypatch, permission_mode):
    """An optional-observer compatibility setting must not admit denied conversation policy."""
    from echo_agent.plugins.manifest import PluginManifest, PluginRecord
    from echo_agent.plugins.runtime import RuntimeExtension

    record = PluginRecord(manifest=PluginManifest(name="connector", permissions=["network"]), source="entrypoint",
                          module={"activate": lambda ctx: ctx.register_runtime_extension(RuntimeExtension())})
    monkeypatch.setattr("echo_agent.plugins.manager.discover_all", lambda **kwargs: [record])
    config = Config(plugins={"required": ["connector"], "permission_mode": permission_mode})
    manager = PluginManager(config=config, workspace=tmp_path, bus=MagicMock(), tool_registry=MagicMock())
    with pytest.raises(RuntimeError, match="required plugin"):
        await manager.discover_and_load()
    assert manager.runtime_extension is None


def test_runtime_ownership_rejects_conflicts_and_releases_only_the_owner():
    """Multiple policy plugins cannot replace each other's native identity context."""
    from echo_agent.plugins.runtime import RuntimeExtension, RuntimeExtensionSlot

    slot = RuntimeExtensionSlot()
    first = RuntimeExtension()
    slot.register("first", first)
    with pytest.raises(RuntimeError, match="already registered"):
        slot.register("second", RuntimeExtension())
    slot.release("second")
    assert slot.extension is first
    slot.release("first")
    assert slot.extension is None


@pytest.mark.asyncio
async def test_runtime_error_prevents_model_inference(tmp_path):
    """Critical conversation callbacks propagate errors rather than silently bypass policy."""
    from types import SimpleNamespace
    from echo_agent.bus.events import InboundEvent
    from echo_agent.plugins.hooks import HookRegistry
    from echo_agent.plugins.runtime import RuntimeExtension
    from tests.test_agent_loop_core import _make_agent_loop

    class RejectingExtension(RuntimeExtension):
        async def handle_event(self, event):
            raise RuntimeError("connector context unavailable")

    loop, _, provider = _make_agent_loop(tmp_path)
    loop.set_plugin_manager(SimpleNamespace(runtime_extension=RejectingExtension(), hooks=HookRegistry()))
    with pytest.raises(RuntimeError, match="connector context unavailable"):
        await loop._process_event(InboundEvent.text_message(channel="test", sender_id="a", chat_id="a", text="hello"), "trace")
    assert provider._call_idx == 0


def test_runtime_object_is_never_serialized_to_config():
    """Runtime secrets/callbacks cannot leak through configuration export."""
    from echo_agent.plugins.runtime import RuntimeExtension

    config = Config()
    config._runtime_extension = RuntimeExtension()
    assert "_runtime_extension" not in config.model_dump()
    assert "_runtime_extension" not in config.model_dump_json()


@pytest.mark.asyncio
@pytest.mark.parametrize("native_allow", [[], ["exec"]])
async def test_required_extension_is_installed_before_native_tool_catalog(tmp_path, monkeypatch, native_allow):
    """A late plugin must both hide disallowed tools and restore allowed native tools."""
    from echo_agent.agent.loop import AgentLoop
    from echo_agent.bus.queue import MessageBus
    from echo_agent.plugins.manifest import PluginManifest, PluginRecord
    from echo_agent.plugins.runtime import RuntimeExtension
    from tests.test_agent_loop_core import _StubProvider

    extension = RuntimeExtension()
    extension.allowed_tool_patterns = ("clarify", "memory", "read_spill")
    record = PluginRecord(manifest=PluginManifest(name="connector", permissions=["hook.register"]), source="entrypoint",
                          module={"activate": lambda ctx: ctx.register_runtime_extension(extension)})
    monkeypatch.setattr("echo_agent.plugins.manager.discover_all", lambda **kwargs: [record])
    config = Config(plugins={"required": ["connector"]}, tools={"profile": "full", "allow": native_allow})
    loop = AgentLoop(bus=MessageBus(), config=config, provider=_StubProvider(), workspace=tmp_path)
    manager = PluginManager(config=config, workspace=tmp_path, bus=loop.bus, tool_registry=loop.tools)
    await manager.discover_and_load()
    loop.set_plugin_manager(manager)
    names = {item["function"]["name"] for item in loop.tools.get_definitions()}
    assert names == {"clarify", "memory", "read_spill"}


@pytest.mark.asyncio
@pytest.mark.parametrize("entry", ["refresh", "discovery", "notification", "reconnect"])
async def test_mcp_extension_policy_error_withdraws_old_tools_and_propagates(tmp_path, entry):
    """Every catalog replacement path must stop exposing stale tools after a policy failure."""
    from unittest.mock import AsyncMock
    from echo_agent.agent.tools.registry import ToolRegistry
    from echo_agent.config.schema import MCPServerConfig
    from echo_agent.mcp.manager import MCPManager
    from echo_agent.plugins.runtime import RuntimeExtension

    class Policy(RuntimeExtension):
        broken = False
        def build_mcp_adapter(self, **kwargs):
            if self.broken:
                raise RuntimeError("adapter policy rejected")
            return super().build_mcp_adapter(**kwargs)

    policy = Policy()
    manager, registry = MCPManager(tmp_path, extension=policy), ToolRegistry()
    client = MagicMock()
    client.is_connected = True
    client.list_tools = AsyncMock(return_value=[{"name": "query"}])
    client.disconnect = AsyncMock()
    manager._clients["catalog"] = client
    manager._configs["catalog"] = MCPServerConfig(url="http://project/mcp")
    await manager.discover_tools(registry)
    assert registry.has("mcp_catalog_query")
    policy.broken = True
    async def reconnect(name, cfg):
        client.is_connected = True
        manager._clients[name] = client
    manager._connect_server = reconnect
    with pytest.raises(RuntimeError, match="adapter policy rejected"):
        if entry == "refresh":
            await manager.refresh_tools("catalog")
        elif entry == "discovery":
            await manager.discover_tools(registry)
        elif entry == "notification":
            await manager._on_notification("catalog", "notifications/tools/list_changed", {})
            # The sole MCP reader must remain available to receive tools/list.
            # Its owned refresh records policy faults for callers after completion.
            await manager._refresh_tasks["catalog"]
            manager.assert_extension_healthy("catalog")
        else:
            client.is_connected = False
            await manager._supervise_once("catalog")
    assert not registry.has("mcp_catalog_query")
    with pytest.raises(RuntimeError, match="adapter policy rejected"):
        manager.assert_extension_healthy()
    with pytest.raises(RuntimeError, match="adapter policy rejected"):
        manager.get_client("catalog")


@pytest.mark.asyncio
async def test_remembered_mcp_policy_fault_blocks_native_handlers_before_any_call(tmp_path):
    """Direct confirmations/attachments cannot bypass a fault by returning before inference."""
    from types import SimpleNamespace
    from unittest.mock import AsyncMock
    from echo_agent.bus.events import InboundEvent
    from echo_agent.mcp.manager import MCPManager
    from echo_agent.plugins.hooks import HookRegistry
    from echo_agent.plugins.runtime import RuntimeExtension, RuntimeExtensionError
    from tests.test_agent_loop_core import _make_agent_loop

    calls = AsyncMock(return_value="executed")
    class NativeExtension(RuntimeExtension):
        async def handle_event(self, event):
            return await calls()

    loop, _, provider = _make_agent_loop(tmp_path)
    loop.set_plugin_manager(SimpleNamespace(runtime_extension=NativeExtension(), hooks=HookRegistry()))
    loop.mcp_manager = MCPManager(tmp_path)
    loop.mcp_manager._extension_errors["catalog"] = RuntimeExtensionError("adapter policy rejected")
    with pytest.raises(RuntimeError, match="adapter policy rejected"):
        await loop._process_event(InboundEvent.text_message(channel="test", sender_id="a", chat_id="a", text="confirm"), "trace")
    calls.assert_not_awaited()
    assert provider._call_idx == 0
