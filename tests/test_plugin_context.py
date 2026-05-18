"""Tests for PluginContext."""

import pytest
from pathlib import Path
from unittest.mock import AsyncMock, MagicMock

from echo_agent.plugins.context import PluginContext
from echo_agent.plugins.hooks import HookRegistry, HookResult
from echo_agent.agent.tools.base import Tool, ToolExecutionContext, ToolResult


class _DummyTool(Tool):
    name = "dummy"
    description = "A dummy tool"
    parameters = {"type": "object", "properties": {}}

    async def execute(self, params, ctx=None):
        return ToolResult(success=True, output="ok")


@pytest.fixture
def mock_deps():
    config = MagicMock()
    bus = MagicMock()
    bus.publish_outbound = AsyncMock()
    tool_registry = MagicMock()
    hook_registry = HookRegistry()
    return config, bus, tool_registry, hook_registry


@pytest.fixture
def ctx(mock_deps, tmp_path):
    config, bus, tool_registry, hook_registry = mock_deps
    return PluginContext(
        plugin_name="test-plugin",
        config=config,
        workspace=tmp_path,
        bus=bus,
        tool_registry=tool_registry,
        hook_registry=hook_registry,
        provider=None,
        plugin_config={"key": "value"},
    )


def test_register_tool(ctx, mock_deps):
    _, _, tool_registry, _ = mock_deps
    tool = _DummyTool()
    ctx.register_tool(tool)
    tool_registry.register.assert_called_once_with(tool)
    assert "dummy" in ctx.registered_tools


def test_register_tool_type_check(ctx):
    with pytest.raises(TypeError, match="Expected a Tool instance"):
        ctx.register_tool("not a tool")


def test_register_tools(ctx, mock_deps):
    _, _, tool_registry, _ = mock_deps
    tools = [_DummyTool(), _DummyTool()]
    tools[1].name = "dummy2"
    ctx.register_tools(tools)
    assert tool_registry.register.call_count == 2
    assert ctx.registered_tools == ["dummy", "dummy2"]


def test_register_hook(ctx, mock_deps):
    _, _, _, hook_registry = mock_deps

    async def my_hook(*args):
        return None

    ctx.register_hook("pre_tool_call", my_hook)
    assert hook_registry.has_hooks("pre_tool_call")
    assert "pre_tool_call" in ctx.registered_hooks


def test_plugin_config(ctx):
    assert ctx.plugin_config == {"key": "value"}


def test_workspace(ctx, tmp_path):
    assert ctx.workspace == tmp_path


def test_config_access(ctx, mock_deps):
    config, _, _, _ = mock_deps
    assert ctx.config is config


def test_llm_provider_none(ctx):
    assert ctx.llm_provider is None


def test_logger(ctx):
    assert ctx.log is not None


def test_plugin_name(ctx):
    assert ctx.plugin_name == "test-plugin"


@pytest.mark.asyncio
async def test_publish_outbound(ctx, mock_deps):
    _, bus, _, _ = mock_deps
    event = MagicMock()
    await ctx.publish_outbound(event)
    bus.publish_outbound.assert_awaited_once_with(event)


def test_subscribe_inbound(ctx, mock_deps):
    _, bus, _, _ = mock_deps

    async def handler(event):
        pass

    ctx.subscribe_inbound(handler)
    bus.subscribe_inbound.assert_called_once_with(handler)


def test_llm_provider_with_value(mock_deps, tmp_path):
    config, bus, tool_registry, hook_registry = mock_deps
    provider = MagicMock()
    ctx = PluginContext(
        plugin_name="test",
        config=config,
        workspace=tmp_path,
        bus=bus,
        tool_registry=tool_registry,
        hook_registry=hook_registry,
        provider=provider,
        plugin_config={},
    )
    assert ctx.llm_provider is provider


def test_plugin_config_default_empty(mock_deps, tmp_path):
    config, bus, tool_registry, hook_registry = mock_deps
    ctx = PluginContext(
        plugin_name="test",
        config=config,
        workspace=tmp_path,
        bus=bus,
        tool_registry=tool_registry,
        hook_registry=hook_registry,
    )
    assert ctx.plugin_config == {}


def test_registered_hooks_returns_copy(ctx):
    async def hook(*args):
        return None

    ctx.register_hook("on_agent_start", hook)
    hooks = ctx.registered_hooks
    hooks.append("fake")
    assert "fake" not in ctx.registered_hooks


def test_registered_tools_returns_copy(ctx, mock_deps):
    _, _, tool_registry, _ = mock_deps
    tool = _DummyTool()
    ctx.register_tool(tool)
    tools = ctx.registered_tools
    tools.append("fake")
    assert "fake" not in ctx.registered_tools
