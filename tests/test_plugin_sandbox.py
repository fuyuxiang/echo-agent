"""Tests for plugin sandbox permission enforcement."""


from echo_agent.plugins.manifest import PluginManifest
from echo_agent.plugins.sandbox import PluginSandbox


def _make_manifest(permissions: list[str] | None = None) -> PluginManifest:
    return PluginManifest(
        name="test-plugin",
        permissions=permissions or [],
    )


class TestPluginSandbox:
    def test_trusted_plugin_always_passes(self):
        manifest = _make_manifest([])
        sandbox = PluginSandbox("test", manifest, trusted=True)
        assert sandbox.check_tool_register() is True
        assert sandbox.check_hook_register() is True
        assert sandbox.check_network() is True

    def test_legacy_plugin_warns_but_allows(self):
        manifest = _make_manifest([])
        sandbox = PluginSandbox("test", manifest, trusted=False)
        assert sandbox.is_legacy is True
        assert sandbox.check_tool_register() is True
        assert sandbox.check_network() is True

    def test_declared_permissions_allow(self):
        manifest = _make_manifest(["tool.register", "hook.register"])
        sandbox = PluginSandbox("test", manifest, trusted=False)
        assert sandbox.is_legacy is False
        assert sandbox.check_tool_register() is True
        assert sandbox.check_hook_register() is True

    def test_undeclared_permission_blocked(self):
        manifest = _make_manifest(["tool.register"])
        sandbox = PluginSandbox("test", manifest, trusted=False)
        assert sandbox.check_tool_register() is True
        assert sandbox.check_hook_register() is False
        assert sandbox.check_network() is False

    def test_violations_tracked(self):
        manifest = _make_manifest(["tool.register"])
        sandbox = PluginSandbox("test", manifest, trusted=False)
        sandbox.check_network()
        sandbox.check_subprocess()
        assert len(sandbox.violations) == 2
        assert "network" in sandbox.violations
        assert "subprocess" in sandbox.violations

    def test_filesystem_permissions(self):
        manifest = _make_manifest(["filesystem.read"])
        sandbox = PluginSandbox("test", manifest, trusted=False)
        assert sandbox.check_filesystem_read() is True
        assert sandbox.check_filesystem_write() is False


def test_plugins_config_default_permission_mode():
    from echo_agent.config.schema import PluginsConfig

    cfg = PluginsConfig()
    assert cfg.permission_mode == "compat"


def test_plugins_config_accepts_strict_mode():
    from echo_agent.config.schema import PluginsConfig

    cfg = PluginsConfig(permission_mode="strict")
    assert cfg.permission_mode == "strict"
