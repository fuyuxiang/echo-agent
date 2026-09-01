"""Tests for PluginManager — full lifecycle."""

import asyncio

import pytest
from unittest.mock import AsyncMock, MagicMock, patch

from echo_agent.agent.tools.registry import ToolRegistry
from echo_agent.bus.queue import MessageBus
from echo_agent.plugins.errors import PluginPermissionError
from echo_agent.plugins.manifest import PluginManifest, PluginRecord
from echo_agent.plugins.manager import PluginManager
from echo_agent.tools import Tool, ToolResult


class _ClosablePluginTool(Tool):
    description = "plugin lifecycle probe"
    parameters = {"type": "object", "properties": {}}

    def __init__(self, name: str, events: list[str]) -> None:
        self.name = name
        self._events = events

    async def execute(self, params, ctx=None):
        return ToolResult(success=True)

    async def aclose(self) -> None:
        self._events.append(f"close:{self.name}")


def _lifecycle_manager(tmp_path) -> tuple[PluginManager, ToolRegistry]:
    registry = ToolRegistry()
    manager = PluginManager(
        config=_make_config(),
        workspace=tmp_path,
        bus=MessageBus(),
        tool_registry=registry,
        provider=None,
    )
    return manager, registry


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
async def test_shutdown_closes_tool_without_deactivate_exactly_once(tmp_path, monkeypatch):
    events: list[str] = []
    tool = _ClosablePluginTool("owned_tool", events)
    record = PluginRecord(manifest=PluginManifest(name="owner"), source="test")
    manager, registry = _lifecycle_manager(tmp_path)

    def activate(ctx):
        ctx.register_tool(tool)

    monkeypatch.setattr(
        "echo_agent.plugins.manager.load_plugin_module",
        lambda _record: {"activate": activate, "deactivate": None},
    )
    await manager._load_and_activate(record)

    await manager.shutdown()
    await manager.shutdown()

    assert events == ["close:owned_tool"]
    assert registry.get("owned_tool") is None


@pytest.mark.asyncio
async def test_shutdown_closes_tool_when_deactivate_raises(tmp_path, monkeypatch):
    events: list[str] = []
    tool = _ClosablePluginTool("fragile_tool", events)
    record = PluginRecord(manifest=PluginManifest(name="fragile"), source="test")
    manager, registry = _lifecycle_manager(tmp_path)

    def activate(ctx):
        ctx.register_tool(tool)

    async def deactivate(_ctx):
        events.append("deactivate:fragile")
        raise RuntimeError("deactivate boom")

    monkeypatch.setattr(
        "echo_agent.plugins.manager.load_plugin_module",
        lambda _record: {"activate": activate, "deactivate": deactivate},
    )
    await manager._load_and_activate(record)
    await manager.shutdown()

    assert events == ["deactivate:fragile", "close:fragile_tool"]
    assert registry.get("fragile_tool") is None


@pytest.mark.asyncio
async def test_shutdown_unwinds_plugins_in_reverse_dependency_order(tmp_path, monkeypatch):
    events: list[str] = []
    base = PluginRecord(manifest=PluginManifest(name="base"), source="test")
    dependent = PluginRecord(
        manifest=PluginManifest(name="dependent", depends_on=["base"]),
        source="test",
    )
    manager, _registry = _lifecycle_manager(tmp_path)
    tools = {
        "base": _ClosablePluginTool("base_tool", events),
        "dependent": _ClosablePluginTool("dependent_tool", events),
    }

    def interface(record):
        name = record.manifest.name

        def activate(ctx):
            ctx.register_tool(tools[name])

        async def deactivate(_ctx):
            events.append(f"deactivate:{name}")

        return {"activate": activate, "deactivate": deactivate}

    monkeypatch.setattr("echo_agent.plugins.manager.load_plugin_module", interface)
    # Same dependency-first order produced by discover_and_load/topological_sort.
    manager._plugins = [base, dependent]
    await manager._load_and_activate(base)
    await manager._load_and_activate(dependent)

    await manager.shutdown()

    assert events == [
        "deactivate:dependent",
        "close:dependent_tool",
        "deactivate:base",
        "close:base_tool",
    ]


@pytest.mark.asyncio
async def test_shutdown_closes_captured_tool_without_removing_replacement(tmp_path, monkeypatch):
    events: list[str] = []
    original = _ClosablePluginTool("replaceable", events)
    replacement = _ClosablePluginTool("replaceable", events)
    record = PluginRecord(manifest=PluginManifest(name="owner"), source="test")
    manager, registry = _lifecycle_manager(tmp_path)

    monkeypatch.setattr(
        "echo_agent.plugins.manager.load_plugin_module",
        lambda _record: {
            "activate": lambda ctx: ctx.register_tool(original),
            "deactivate": None,
        },
    )
    await manager._load_and_activate(record)
    assert manager.owns_tool(original) is True
    assert manager.owned_tool_identities == frozenset({id(original)})
    registry.register(replacement, replace=True)
    assert manager.owns_tool(replacement) is False

    await manager.shutdown()

    assert events == ["close:replaceable"]
    assert registry.get("replaceable") is replacement
    assert manager.owns_tool(original) is False
    assert manager.owned_tool_identities == frozenset()


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
        "from echo_agent.tools import Tool, ToolResult\n"
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
async def test_compat_mode_blocks_tool_registered_without_permission(tmp_path, monkeypatch):
    """compat denies visibility without failing the otherwise usable plugin."""
    from echo_agent.config.schema import Config
    from echo_agent.plugins.manifest import PluginManifest, PluginRecord, PluginProvides

    events: list[str] = []
    fake_tool = _ClosablePluginTool("test_tool", events)
    visible_during_activation: list[bool] = []
    tool_registry = ToolRegistry()

    def fake_activate(ctx):
        ctx.register_tool(fake_tool)
        visible_during_activation.append(tool_registry.get("test_tool") is not None)

    # 插件声明了 hook.register 权限，但 provides.tools=["test_tool"]，没有 tool.register
    # => check_tool_register() 会因缺少 tool.register 权限返回 False
    # => PluginContext 在写入全局 registry 前阻断，并由 manager 回收实例
    manifest = PluginManifest(
        name="compat-strip-plugin",
        permissions=["hook.register"],
        provides=PluginProvides(tools=["test_tool"]),
    )
    record = PluginRecord(manifest=manifest, source="user")

    cfg = Config()
    cfg.plugins.permission_mode = "compat"

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
    assert visible_during_activation == [False]
    assert tool_registry.get("test_tool") is None
    assert record.tools_registered == []
    assert events == ["close:test_tool"]


@pytest.mark.asyncio
async def test_activation_failure_rolls_back_every_context_resource(
    tmp_path, monkeypatch,
):
    events: list[str] = []
    tool = _ClosablePluginTool("partial_tool", events)
    record = PluginRecord(manifest=PluginManifest(name="partial"), source="test")
    manager, registry = _lifecycle_manager(tmp_path)

    async def inbound_handler(_event):
        return None

    async def hook_callback(*_args, **_kwargs):
        return None

    def activate(ctx):
        ctx.register_tool(tool)
        ctx.register_hook("on_agent_start", hook_callback)
        ctx.subscribe_inbound(inbound_handler)
        raise RuntimeError("partial activation")

    monkeypatch.setattr(
        "echo_agent.plugins.manager.load_plugin_module",
        lambda _record: {"activate": activate, "deactivate": None},
    )

    await manager._load_and_activate(record)

    assert record.status == "failed"
    assert events == ["close:partial_tool"]
    assert registry.get("partial_tool") is None
    assert manager.hooks.has_hooks("on_agent_start") is False
    assert inbound_handler not in manager._bus._inbound_subscribers
    assert "partial" not in manager._contexts


@pytest.mark.asyncio
async def test_registry_rejection_still_closes_plugin_tool(tmp_path, monkeypatch):
    """Ownership begins before registry admission, not after it succeeds."""
    existing_events: list[str] = []
    rejected_events: list[str] = []
    builtin = _ClosablePluginTool("shared_name", existing_events)
    rejected = _ClosablePluginTool("shared_name", rejected_events)
    record = PluginRecord(manifest=PluginManifest(name="collision"), source="test")
    manager, registry = _lifecycle_manager(tmp_path)
    registry.register(builtin)

    monkeypatch.setattr(
        "echo_agent.plugins.manager.load_plugin_module",
        lambda _record: {
            "activate": lambda ctx: ctx.register_tool(rejected),
            "deactivate": None,
        },
    )

    await manager._load_and_activate(record)

    assert record.status == "failed"
    assert "already registered" in record.error
    assert registry.get("shared_name") is builtin
    assert existing_events == []
    assert rejected_events == ["close:shared_name"]


@pytest.mark.asyncio
async def test_caught_registry_rejection_is_closed_on_successful_plugin_shutdown(
    tmp_path, monkeypatch,
):
    existing_events: list[str] = []
    rejected_events: list[str] = []
    builtin = _ClosablePluginTool("shared_name", existing_events)
    rejected = _ClosablePluginTool("shared_name", rejected_events)
    record = PluginRecord(manifest=PluginManifest(name="collision-caught"), source="test")
    manager, registry = _lifecycle_manager(tmp_path)
    registry.register(builtin)

    def activate(ctx):
        try:
            ctx.register_tool(rejected)
        except ValueError:
            # Compatibility plugins may probe an optional name and continue.
            pass

    monkeypatch.setattr(
        "echo_agent.plugins.manager.load_plugin_module",
        lambda _record: {"activate": activate, "deactivate": None},
    )

    await manager._load_and_activate(record)
    assert record.status == "activated"
    assert rejected_events == []

    await manager.shutdown()

    assert registry.get("shared_name") is builtin
    assert existing_events == []
    assert rejected_events == ["close:shared_name"]


@pytest.mark.asyncio
async def test_shutdown_unsubscribes_plugin_inbound_handler(tmp_path, monkeypatch):
    record = PluginRecord(manifest=PluginManifest(name="observer"), source="test")
    manager, _registry = _lifecycle_manager(tmp_path)

    async def inbound_handler(_event):
        return None

    monkeypatch.setattr(
        "echo_agent.plugins.manager.load_plugin_module",
        lambda _record: {
            "activate": lambda ctx: ctx.subscribe_inbound(inbound_handler),
            "deactivate": None,
        },
    )

    await manager._load_and_activate(record)
    assert inbound_handler in manager._bus._inbound_subscribers

    await manager.shutdown()

    assert inbound_handler not in manager._bus._inbound_subscribers


@pytest.mark.asyncio
async def test_cancelled_activation_finishes_rollback_before_propagating(
    tmp_path, monkeypatch,
):
    events: list[str] = []
    close_started = asyncio.Event()
    release_close = asyncio.Event()

    class SlowCloseTool(_ClosablePluginTool):
        async def aclose(self) -> None:
            close_started.set()
            await release_close.wait()
            await super().aclose()

    tool = SlowCloseTool("cancelled_tool", events)
    record = PluginRecord(manifest=PluginManifest(name="cancelled"), source="test")
    manager, registry = _lifecycle_manager(tmp_path)
    entered = asyncio.Event()

    async def inbound_handler(_event):
        return None

    async def hook_callback(*_args, **_kwargs):
        return None

    async def activate(ctx):
        ctx.register_tool(tool)
        ctx.register_hook("on_agent_start", hook_callback)
        ctx.subscribe_inbound(inbound_handler)
        entered.set()
        await asyncio.Future()

    monkeypatch.setattr(
        "echo_agent.plugins.manager.load_plugin_module",
        lambda _record: {"activate": activate, "deactivate": None},
    )

    activation = asyncio.create_task(manager._load_and_activate(record))
    await entered.wait()
    activation.cancel()
    await close_started.wait()
    activation.cancel()
    await asyncio.sleep(0)
    assert activation.done() is False
    release_close.set()
    with pytest.raises(asyncio.CancelledError):
        await activation

    assert record.status == "failed"
    assert events == ["close:cancelled_tool"]
    assert registry.get("cancelled_tool") is None
    assert manager.hooks.has_hooks("on_agent_start") is False
    assert inbound_handler not in manager._bus._inbound_subscribers


@pytest.mark.asyncio
@pytest.mark.parametrize("asynchronous", [False, True], ids=["sync", "async"])
async def test_strict_mode_checks_actual_undeclared_registrations(
    tmp_path, monkeypatch, asynchronous,
):
    events: list[str] = []
    tool = _ClosablePluginTool("undeclared_tool", events)
    record = PluginRecord(
        manifest=PluginManifest(name="undeclared", permissions=[]),
        source="test",
    )
    config = _make_config()
    config.plugins.permission_mode = "strict"
    registry = ToolRegistry()
    manager = PluginManager(
        config=config,
        workspace=tmp_path,
        bus=MessageBus(),
        tool_registry=registry,
    )
    activated = False
    visible_during_activation: list[bool] = []

    async def hook_callback(*_args, **_kwargs):
        return None

    def register_resources(ctx):
        nonlocal activated
        activated = True
        try:
            ctx.register_tool(tool)
        except PluginPermissionError:
            pass
        visible_during_activation.append(registry.get("undeclared_tool") is not None)
        try:
            ctx.register_hook("on_agent_start", hook_callback)
        except PluginPermissionError:
            pass
        visible_during_activation.append(manager.hooks.has_hooks("on_agent_start"))

    async def async_activate(ctx):
        register_resources(ctx)

    activate = async_activate if asynchronous else register_resources

    monkeypatch.setattr(
        "echo_agent.plugins.manager.load_plugin_module",
        lambda _record: {"activate": activate, "deactivate": None},
    )

    await manager._load_and_activate(record)

    assert activated is True
    assert record.status == "failed"
    assert "actual registrations" in record.error
    assert visible_during_activation == [False, False]
    assert events == ["close:undeclared_tool"]
    assert registry.get("undeclared_tool") is None
    assert manager.hooks.has_hooks("on_agent_start") is False


@pytest.mark.asyncio
async def test_unknown_permission_rejected_before_import(tmp_path, monkeypatch):
    record = PluginRecord(
        manifest=PluginManifest(name="typo", permissions=["filesystem"]),
        source="test",
    )
    manager, _registry = _lifecycle_manager(tmp_path)
    loader = MagicMock()
    monkeypatch.setattr("echo_agent.plugins.manager.load_plugin_module", loader)

    await manager._load_and_activate(record)

    assert record.status == "failed"
    assert record.error == "unknown permissions: filesystem"
    loader.assert_not_called()


@pytest.mark.asyncio
async def test_missing_dependency_is_not_activated(tmp_path, monkeypatch):
    record = PluginRecord(
        manifest=PluginManifest(name="orphan", depends_on=["missing"]),
        source="test",
    )
    manager, _registry = _lifecycle_manager(tmp_path)
    loader = MagicMock()
    monkeypatch.setattr(
        "echo_agent.plugins.manager.discover_all", lambda **_kwargs: [record],
    )
    monkeypatch.setattr("echo_agent.plugins.manager.load_plugin_module", loader)

    await manager.discover_and_load()

    assert record.status == "failed"
    assert "missing dependencies" in record.error
    loader.assert_not_called()


@pytest.mark.asyncio
async def test_dependency_cycle_is_not_activated(tmp_path, monkeypatch):
    first = PluginRecord(
        manifest=PluginManifest(name="first", depends_on=["second"]),
        source="test",
    )
    second = PluginRecord(
        manifest=PluginManifest(name="second", depends_on=["first"]),
        source="test",
    )
    manager, _registry = _lifecycle_manager(tmp_path)
    loader = MagicMock()
    monkeypatch.setattr(
        "echo_agent.plugins.manager.discover_all",
        lambda **_kwargs: [first, second],
    )
    monkeypatch.setattr("echo_agent.plugins.manager.load_plugin_module", loader)

    await manager.discover_and_load()

    assert first.status == second.status == "failed"
    assert "dependency cycle" in first.error
    assert "dependency cycle" in second.error
    loader.assert_not_called()


@pytest.mark.asyncio
async def test_dependent_skipped_when_base_activation_fails(tmp_path, monkeypatch):
    base = PluginRecord(manifest=PluginManifest(name="base"), source="test")
    dependent = PluginRecord(
        manifest=PluginManifest(name="dependent", depends_on=["base"]),
        source="test",
    )
    manager, _registry = _lifecycle_manager(tmp_path)
    activation_attempts: list[str] = []

    def interface(record):
        name = record.manifest.name

        def activate(_ctx):
            activation_attempts.append(name)
            if name == "base":
                raise RuntimeError("base failed")

        return {"activate": activate, "deactivate": None}

    monkeypatch.setattr(
        "echo_agent.plugins.manager.discover_all",
        lambda **_kwargs: [dependent, base],
    )
    monkeypatch.setattr("echo_agent.plugins.manager.load_plugin_module", interface)

    await manager.discover_and_load()

    assert activation_attempts == ["base"]
    assert base.status == "failed"
    assert dependent.status == "failed"
    assert dependent.error == "dependencies not activated: base"
