"""Tests for plugin loader — discovery and module loading."""

import sys
import pytest
from pathlib import Path
from unittest.mock import patch, MagicMock

from echo_agent.plugins.loader import (
    discover_all,
    load_plugin_module,
    topological_sort,
    _scan_directory,
    _scan_entry_points,
    _resolve_plugin_interface,
)
from echo_agent.plugins.manifest import PluginManifest, PluginRecord
from echo_agent.plugins.errors import PluginLoadError


@pytest.fixture
def plugin_dir(tmp_path):
    """Create a valid plugin directory."""
    d = tmp_path / "plugins" / "my-plugin"
    d.mkdir(parents=True)
    (d / "plugin.yaml").write_text("name: my-plugin\nversion: '1.0.0'\n")
    (d / "__init__.py").write_text(
        "async def activate(ctx): pass\n"
        "plugin = {'activate': activate}\n"
    )
    return tmp_path / "plugins"


def test_scan_directory(plugin_dir):
    records = _scan_directory(plugin_dir, source="user")
    assert len(records) == 1
    assert records[0].manifest.name == "my-plugin"
    assert records[0].source == "user"


def test_scan_directory_nonexistent(tmp_path):
    records = _scan_directory(tmp_path / "nonexistent", source="user")
    assert records == []


def test_scan_directory_no_manifest(tmp_path):
    d = tmp_path / "plugins" / "no-manifest"
    d.mkdir(parents=True)
    (d / "__init__.py").write_text("pass\n")
    records = _scan_directory(tmp_path / "plugins", source="user")
    assert records == []


def test_discover_all_project_dir(plugin_dir, tmp_path):
    workspace = tmp_path
    (workspace / "plugins").mkdir(exist_ok=True)
    proj_plugin = workspace / "plugins" / "proj-plugin"
    proj_plugin.mkdir()
    (proj_plugin / "plugin.yaml").write_text("name: proj-plugin\nversion: '0.1.0'\n")
    (proj_plugin / "__init__.py").write_text("async def activate(ctx): pass\nplugin = {'activate': activate}\n")

    with patch("echo_agent.plugins.loader._scan_entry_points", return_value=[]):
        records = discover_all(workspace=workspace)

    names = [r.manifest.name for r in records]
    assert "proj-plugin" in names


def test_load_plugin_module_directory(plugin_dir):
    records = _scan_directory(plugin_dir, source="user")
    record = records[0]
    interface = load_plugin_module(record)
    assert "activate" in interface
    assert callable(interface["activate"])


def test_load_plugin_module_missing_init(tmp_path):
    d = tmp_path / "bad-plugin"
    d.mkdir()
    (d / "plugin.yaml").write_text("name: bad-plugin\n")
    record = PluginRecord(
        manifest=PluginManifest(name="bad-plugin"),
        source="user",
        path=d,
    )
    with pytest.raises(PluginLoadError, match="Missing __init__.py"):
        load_plugin_module(record)


def test_resolve_plugin_interface_dict():
    async def act(ctx):
        pass

    result = _resolve_plugin_interface("test", {"activate": act})
    assert result["activate"] is act
    assert result["deactivate"] is None


def test_resolve_plugin_interface_no_activate():
    with pytest.raises(PluginLoadError, match="No activate"):
        _resolve_plugin_interface("test", {"deactivate": lambda ctx: None})


def test_topological_sort_simple():
    a = PluginRecord(manifest=PluginManifest(name="a", depends_on=["b"]), source="user")
    b = PluginRecord(manifest=PluginManifest(name="b"), source="user")
    result = topological_sort([a, b])
    names = [r.manifest.name for r in result]
    assert names.index("b") < names.index("a")


def test_topological_sort_circular():
    a = PluginRecord(manifest=PluginManifest(name="a", depends_on=["b"]), source="user")
    b = PluginRecord(manifest=PluginManifest(name="b", depends_on=["a"]), source="user")
    result = topological_sort([a, b])
    assert len(result) >= 1


def test_topological_sort_no_deps():
    a = PluginRecord(manifest=PluginManifest(name="a"), source="user")
    b = PluginRecord(manifest=PluginManifest(name="b"), source="user")
    result = topological_sort([a, b])
    assert len(result) == 2


@patch("echo_agent.plugins.loader.importlib.metadata.entry_points")
def test_scan_entry_points_mock(mock_eps):
    mock_ep = MagicMock()
    mock_ep.name = "mock-plugin"
    mock_ep.load.return_value = {
        "activate": lambda ctx: None,
        "manifest": {"name": "mock-plugin", "version": "0.5.0"},
    }
    mock_eps.return_value = [mock_ep]

    records = _scan_entry_points()
    assert len(records) == 1
    assert records[0].manifest.name == "mock-plugin"
    assert records[0].manifest.version == "0.5.0"
    assert records[0].source == "entrypoint"


def test_scan_directory_plugin_yml(tmp_path):
    """Should also find plugin.yml (not just plugin.yaml)."""
    d = tmp_path / "plugins" / "yml-plugin"
    d.mkdir(parents=True)
    (d / "plugin.yml").write_text("name: yml-plugin\nversion: '2.0.0'\n")
    (d / "__init__.py").write_text("async def activate(ctx): pass\nplugin = {'activate': activate}\n")
    records = _scan_directory(tmp_path / "plugins", source="user")
    assert len(records) == 1
    assert records[0].manifest.name == "yml-plugin"


def test_discover_all_override_semantics(tmp_path):
    """Project plugins override user plugins with the same name."""
    user_dir = Path.home() / ".echo-agent" / "plugins"

    proj_dir = tmp_path / "plugins" / "shared-name"
    proj_dir.mkdir(parents=True)
    (proj_dir / "plugin.yaml").write_text("name: shared-name\nversion: '2.0.0'\n")
    (proj_dir / "__init__.py").write_text("async def activate(ctx): pass\nplugin = {'activate': activate}\n")

    with patch("echo_agent.plugins.loader._scan_entry_points", return_value=[]):
        with patch("echo_agent.plugins.loader._scan_directory") as mock_scan:
            user_record = PluginRecord(
                manifest=PluginManifest(name="shared-name", version="1.0.0"),
                source="user",
                path=tmp_path / "user-path",
            )
            proj_record = PluginRecord(
                manifest=PluginManifest(name="shared-name", version="2.0.0"),
                source="project",
                path=proj_dir,
            )
            mock_scan.side_effect = [[user_record], [proj_record]]
            records = discover_all(workspace=tmp_path)

    assert len(records) == 1
    assert records[0].manifest.version == "2.0.0"
    assert records[0].source == "project"


def test_discover_all_extra_dirs(tmp_path):
    """Extra dirs from config should be scanned."""
    extra = tmp_path / "extra-plugins" / "extra-one"
    extra.mkdir(parents=True)
    (extra / "plugin.yaml").write_text("name: extra-one\n")
    (extra / "__init__.py").write_text("async def activate(ctx): pass\nplugin = {'activate': activate}\n")

    with patch("echo_agent.plugins.loader._scan_entry_points", return_value=[]):
        records = discover_all(workspace=tmp_path, extra_dirs=[str(tmp_path / "extra-plugins")])

    names = [r.manifest.name for r in records]
    assert "extra-one" in names


def test_load_plugin_module_entrypoint():
    """Entry point plugins should resolve interface from the loaded module."""
    async def act(ctx):
        pass

    record = PluginRecord(
        manifest=PluginManifest(name="ep-plugin"),
        source="entrypoint",
        module={"activate": act, "deactivate": None},
    )
    interface = load_plugin_module(record)
    assert interface["activate"] is act


def test_load_plugin_module_no_path():
    """Directory plugin without path should raise."""
    record = PluginRecord(
        manifest=PluginManifest(name="no-path"),
        source="user",
        path=None,
    )
    with pytest.raises(PluginLoadError, match="No path"):
        load_plugin_module(record)


def test_manifest_from_entrypoint_module():
    """Module with plugin dict containing manifest."""
    import types
    mod = types.ModuleType("test_mod")
    mod.plugin = {
        "activate": lambda ctx: None,
        "manifest": {"name": "mod-plugin", "version": "3.0.0"},
    }
    from echo_agent.plugins.loader import _manifest_from_entrypoint
    m = _manifest_from_entrypoint("fallback-name", mod)
    assert m.name == "mod-plugin"
    assert m.version == "3.0.0"


def test_manifest_from_entrypoint_module_with_MANIFEST():
    """Module with MANIFEST attribute."""
    import types
    mod = types.ModuleType("test_mod2")
    mod.MANIFEST = {"name": "manifest-plugin", "version": "4.0.0"}
    from echo_agent.plugins.loader import _manifest_from_entrypoint
    m = _manifest_from_entrypoint("fallback", mod)
    assert m.name == "manifest-plugin"


def test_manifest_from_entrypoint_module_fallback():
    """Module without plugin or MANIFEST falls back to name."""
    import types
    mod = types.ModuleType("bare_mod")
    from echo_agent.plugins.loader import _manifest_from_entrypoint
    m = _manifest_from_entrypoint("bare-name", mod)
    assert m.name == "bare-name"


def test_resolve_plugin_interface_module_with_activate():
    """Module with top-level activate function (no plugin dict)."""
    import types
    mod = types.ModuleType("direct_mod")

    async def act(ctx):
        pass

    mod.activate = act
    mod.deactivate = None
    result = _resolve_plugin_interface("test", mod)
    assert result["activate"] is act


def test_resolve_plugin_interface_unexpected_type():
    """Non-dict, non-module target should raise."""
    with pytest.raises(PluginLoadError, match="Unexpected plugin target type"):
        _resolve_plugin_interface("test", 42)


def test_topological_sort_missing_dep():
    """Dependencies not in the record set should be silently skipped."""
    a = PluginRecord(manifest=PluginManifest(name="a", depends_on=["missing"]), source="user")
    result = topological_sort([a])
    assert len(result) == 1
    assert result[0].manifest.name == "a"
