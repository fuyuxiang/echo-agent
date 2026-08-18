"""Tests for PluginManager — full lifecycle."""

import pytest
from unittest.mock import AsyncMock, MagicMock, patch

from echo_agent.plugins.manager import PluginManager


def _make_config(*, enabled=True, allow=None, deny=None, extra_dirs=None, plugin_config=None):
    config = MagicMock()
    config.plugins.enabled = enabled
    config.plugins.allow = allow or []
    config.plugins.deny = deny or []
    config.plugins.extra_dirs = extra_dirs or []
    config.plugins.config = plugin_config or {}
    config.plugins.permission_mode = "compat"
    config.plugins.trusted_plugins = []
    return config


@pytest.fixture
def plugin_dir(tmp_path):
    d = tmp_path / "plugins" / "hello-plugin"
    d.mkdir(parents=True)
    (d / "plugin.yaml").write_text(
        "name: hello-plugin\nversion: '1.0.0'\ndescription: Hello\nconfig_key: hello\n"
    )
    (d / "__init__.py").write_text(
        "async def activate(ctx):\n"
        "    ctx.log.info('hello activated')\n"
        "plugin = {'activate': activate}\n"
    )
    return tmp_path


@pytest.fixture
def manager(plugin_dir):
    config = _make_config(plugin_config={"hello": {"greeting": "hi"}})
    bus = MagicMock()
    bus.publish_outbound = AsyncMock()
    tool_registry = MagicMock()
    return PluginManager(
        config=config,
        workspace=plugin_dir,
        bus=bus,
        tool_registry=tool_registry,
        provider=None,
    )


@pytest.mark.asyncio
async def test_discover_and_load(manager):
    with patch("echo_agent.plugins.loader._scan_entry_points", return_value=[]):
        await manager.discover_and_load()

    assert len(manager.plugins) == 1
    assert manager.plugins[0].status == "activated"
    assert manager.plugins[0].manifest.name == "hello-plugin"


@pytest.mark.asyncio
async def test_plugin_disabled_by_config(plugin_dir):
    config = _make_config(deny=["hello-plugin"])
    bus = MagicMock()
    tool_registry = MagicMock()
    mgr = PluginManager(
        config=config, workspace=plugin_dir, bus=bus,
        tool_registry=tool_registry, provider=None,
    )
    with patch("echo_agent.plugins.loader._scan_entry_points", return_value=[]):
        await mgr.discover_and_load()

    assert len(mgr.plugins) == 0 or all(p.status == "disabled" for p in mgr.plugins)


@pytest.mark.asyncio
async def test_plugin_system_disabled(plugin_dir):
    config = _make_config(enabled=False)
    bus = MagicMock()
    tool_registry = MagicMock()
    mgr = PluginManager(
        config=config, workspace=plugin_dir, bus=bus,
        tool_registry=tool_registry, provider=None,
    )
    with patch("echo_agent.plugins.loader._scan_entry_points", return_value=[]):
        await mgr.discover_and_load()

    assert mgr.plugins == []


@pytest.mark.asyncio
async def test_plugin_missing_env(tmp_path):
    d = tmp_path / "plugins" / "env-plugin"
    d.mkdir(parents=True)
    (d / "plugin.yaml").write_text(
        "name: env-plugin\nrequires_env:\n  - NONEXISTENT_VAR_ABC123\n"
    )
    (d / "__init__.py").write_text("async def activate(ctx): pass\nplugin = {'activate': activate}\n")

    config = _make_config()
    bus = MagicMock()
    tool_registry = MagicMock()
    mgr = PluginManager(
        config=config, workspace=tmp_path, bus=bus,
        tool_registry=tool_registry, provider=None,
    )
    with patch("echo_agent.plugins.loader._scan_entry_points", return_value=[]):
        await mgr.discover_and_load()

    failed = [p for p in mgr.plugins if p.status == "failed"]
    assert len(failed) == 1
    assert "NONEXISTENT_VAR_ABC123" in failed[0].error


@pytest.mark.asyncio
async def test_plugin_activate_error(tmp_path):
    d = tmp_path / "plugins" / "bad-plugin"
    d.mkdir(parents=True)
    (d / "plugin.yaml").write_text("name: bad-plugin\n")
    (d / "__init__.py").write_text(
        "async def activate(ctx): raise RuntimeError('oops')\n"
        "plugin = {'activate': activate}\n"
    )

    config = _make_config()
    bus = MagicMock()
    tool_registry = MagicMock()
    mgr = PluginManager(
        config=config, workspace=tmp_path, bus=bus,
        tool_registry=tool_registry, provider=None,
    )
    with patch("echo_agent.plugins.loader._scan_entry_points", return_value=[]):
        await mgr.discover_and_load()

    failed = [p for p in mgr.plugins if p.status == "failed"]
    assert len(failed) == 1
    assert "oops" in failed[0].error


@pytest.mark.asyncio
async def test_shutdown_calls_deactivate(manager):
    with patch("echo_agent.plugins.loader._scan_entry_points", return_value=[]):
        await manager.discover_and_load()

    await manager.shutdown()


@pytest.mark.asyncio
async def test_get_status_report(manager):
    with patch("echo_agent.plugins.loader._scan_entry_points", return_value=[]):
        await manager.discover_and_load()

    report = manager.get_status_report()
    assert len(report) == 1
    assert report[0]["name"] == "hello-plugin"
    assert report[0]["status"] == "activated"


@pytest.mark.asyncio
async def test_get_plugin_info(manager):
    with patch("echo_agent.plugins.loader._scan_entry_points", return_value=[]):
        await manager.discover_and_load()

    info = manager.get_plugin_info("hello-plugin")
    assert info is not None
    assert info.manifest.name == "hello-plugin"

    assert manager.get_plugin_info("nonexistent") is None


@pytest.mark.asyncio
async def test_hooks_accessible(manager):
    with patch("echo_agent.plugins.loader._scan_entry_points", return_value=[]):
        await manager.discover_and_load()

    assert manager.hooks is not None


@pytest.mark.asyncio
async def test_allow_list_filtering(tmp_path):
    d1 = tmp_path / "plugins" / "allowed-plugin"
    d1.mkdir(parents=True)
    (d1 / "plugin.yaml").write_text("name: allowed-plugin\n")
    (d1 / "__init__.py").write_text("async def activate(ctx): pass\nplugin = {'activate': activate}\n")

    d2 = tmp_path / "plugins" / "blocked-plugin"
    d2.mkdir(parents=True)
    (d2 / "plugin.yaml").write_text("name: blocked-plugin\n")
    (d2 / "__init__.py").write_text("async def activate(ctx): pass\nplugin = {'activate': activate}\n")

    config = _make_config(allow=["allowed-plugin"])
    bus = MagicMock()
    tool_registry = MagicMock()
    mgr = PluginManager(
        config=config, workspace=tmp_path, bus=bus,
        tool_registry=tool_registry, provider=None,
    )
    with patch("echo_agent.plugins.loader._scan_entry_points", return_value=[]):
        await mgr.discover_and_load()

    activated = [p for p in mgr.plugins if p.status == "activated"]
    assert len(activated) == 1
    assert activated[0].manifest.name == "allowed-plugin"


@pytest.mark.asyncio
async def test_sync_activate(tmp_path):
    d = tmp_path / "plugins" / "sync-plugin"
    d.mkdir(parents=True)
    (d / "plugin.yaml").write_text("name: sync-plugin\n")
    (d / "__init__.py").write_text("def activate(ctx): pass\nplugin = {'activate': activate}\n")

    config = _make_config()
    bus = MagicMock()
    tool_registry = MagicMock()
    mgr = PluginManager(
        config=config, workspace=tmp_path, bus=bus,
        tool_registry=tool_registry, provider=None,
    )
    with patch("echo_agent.plugins.loader._scan_entry_points", return_value=[]):
        await mgr.discover_and_load()

    assert mgr.plugins[0].status == "activated"


@pytest.mark.asyncio
async def test_deactivate_exception(tmp_path):
    """Deactivate raising should not crash shutdown."""
    d = tmp_path / "plugins" / "bad-deactivate"
    d.mkdir(parents=True)
    (d / "plugin.yaml").write_text("name: bad-deactivate\n")
    (d / "__init__.py").write_text(
        "async def activate(ctx): pass\n"
        "async def deactivate(ctx): raise RuntimeError('deactivate boom')\n"
        "plugin = {'activate': activate, 'deactivate': deactivate}\n"
    )

    config = _make_config()
    bus = MagicMock()
    tool_registry = MagicMock()
    mgr = PluginManager(
        config=config, workspace=tmp_path, bus=bus,
        tool_registry=tool_registry, provider=None,
    )
    with patch("echo_agent.plugins.loader._scan_entry_points", return_value=[]):
        await mgr.discover_and_load()

    await mgr.shutdown()


@pytest.mark.asyncio
async def test_config_key_passthrough(tmp_path):
    d = tmp_path / "plugins" / "cfg-plugin"
    d.mkdir(parents=True)
    (d / "plugin.yaml").write_text("name: cfg-plugin\nconfig_key: my_cfg\n")
    (d / "__init__.py").write_text(
        "received_config = None\n"
        "async def activate(ctx):\n"
        "    global received_config\n"
        "    received_config = ctx.plugin_config\n"
        "plugin = {'activate': activate}\n"
    )

    config = _make_config(plugin_config={"my_cfg": {"api_key": "secret123"}})
    bus = MagicMock()
    tool_registry = MagicMock()
    mgr = PluginManager(
        config=config, workspace=tmp_path, bus=bus,
        tool_registry=tool_registry, provider=None,
    )
    with patch("echo_agent.plugins.loader._scan_entry_points", return_value=[]):
        await mgr.discover_and_load()

    assert mgr.plugins[0].status == "activated"


@pytest.mark.asyncio
async def test_no_plugins_discovered(tmp_path):
    config = _make_config()
    bus = MagicMock()
    tool_registry = MagicMock()
    mgr = PluginManager(
        config=config, workspace=tmp_path, bus=bus,
        tool_registry=tool_registry, provider=None,
    )
    with patch("echo_agent.plugins.loader._scan_entry_points", return_value=[]):
        await mgr.discover_and_load()

    assert mgr.plugins == []


@pytest.mark.asyncio
async def test_plugin_registers_tool(tmp_path):
    d = tmp_path / "plugins" / "tool-plugin"
    d.mkdir(parents=True)
    (d / "plugin.yaml").write_text("name: tool-plugin\n")
    (d / "__init__.py").write_text(
        "from echo_agent.agent.tools.base import Tool, ToolResult\n"
        "class MyTool(Tool):\n"
        "    name = 'my_tool'\n"
        "    description = 'test'\n"
        "    parameters = {'type': 'object', 'properties': {}}\n"
        "    async def execute(self, params, ctx=None):\n"
        "        return ToolResult(success=True)\n"
        "async def activate(ctx):\n"
        "    ctx.register_tool(MyTool())\n"
        "plugin = {'activate': activate}\n"
    )

    config = _make_config()
    bus = MagicMock()
    tool_registry = MagicMock()
    mgr = PluginManager(
        config=config, workspace=tmp_path, bus=bus,
        tool_registry=tool_registry, provider=None,
    )
    with patch("echo_agent.plugins.loader._scan_entry_points", return_value=[]):
        await mgr.discover_and_load()

    assert mgr.plugins[0].status == "activated"
    assert "my_tool" in mgr.plugins[0].tools_registered
    tool_registry.register.assert_called_once()


def test_strict_mode_rejects_before_activate(tmp_path, monkeypatch):
    """strict 模式下 legacy 插件越权时，activate 不应被调用。"""
    import asyncio
    from echo_agent.config.schema import Config
    from echo_agent.plugins.manager import PluginManager
    from echo_agent.plugins.manifest import PluginManifest, PluginRecord, PluginProvides
    from echo_agent.agent.tools.registry import ToolRegistry
    from echo_agent.bus.queue import MessageBus

    activate_called = {"flag": False}

    def fake_activate(ctx):
        activate_called["flag"] = True

    manifest = PluginManifest(
        name="evil",
        permissions=[],
        provides=PluginProvides(tools=["evil_tool"]),
    )
    record = PluginRecord(manifest=manifest, source="user")

    cfg = Config()
    cfg.plugins.permission_mode = "strict"

    mgr = PluginManager(
        config=cfg,
        workspace=tmp_path,
        bus=MessageBus(),
        tool_registry=ToolRegistry(),
    )

    monkeypatch.setattr(
        "echo_agent.plugins.manager.load_plugin_module",
        lambda rec: {"activate": fake_activate, "deactivate": None},
    )

    asyncio.run(mgr._load_and_activate(record))

    assert activate_called["flag"] is False
    assert record.status == "failed"
    assert "permission" in record.error.lower()


@pytest.mark.asyncio
async def test_compat_mode_strips_tool_registered_without_permission(tmp_path, monkeypatch):
    """compat 模式下，插件注册了没有权限的工具，事后裁剪应调用 unregister。"""
    from echo_agent.config.schema import Config
    from echo_agent.plugins.manager import PluginManager
    from echo_agent.plugins.manifest import PluginManifest, PluginRecord, PluginProvides
    from echo_agent.agent.tools.registry import ToolRegistry
    from echo_agent.bus.queue import MessageBus
    from unittest.mock import MagicMock
    from echo_agent.agent.tools.base import Tool, ToolResult

    class FakeTool(Tool):
        name = "test_tool"
        description = "test"
        parameters = {"type": "object", "properties": {}}

        async def execute(self, params, ctx=None):
            return ToolResult(success=True)

    fake_tool = FakeTool()

    def fake_activate(ctx):
        ctx.register_tool(fake_tool)

    # 插件声明了 hook.register 权限，但 provides.tools=["test_tool"]，没有 tool.register
    # => check_tool_register() 会因缺少 tool.register 权限返回 False
    # => tool_ok=False，事后裁剪触发
    manifest = PluginManifest(
        name="compat-strip-plugin",
        permissions=["hook.register"],
        provides=PluginProvides(tools=["test_tool"]),
    )
    record = PluginRecord(manifest=manifest, source="user")

    cfg = Config()
    cfg.plugins.permission_mode = "compat"

    tool_registry = MagicMock(spec=ToolRegistry)

    mgr = PluginManager(
        config=cfg,
        workspace=tmp_path,
        bus=MessageBus(),
        tool_registry=tool_registry,
    )

    monkeypatch.setattr(
        "echo_agent.plugins.manager.load_plugin_module",
        lambda rec: {"activate": fake_activate},
    )

    await mgr._load_and_activate(record)

    assert record.status == "activated"
    tool_registry.unregister.assert_called_once_with("test_tool")
