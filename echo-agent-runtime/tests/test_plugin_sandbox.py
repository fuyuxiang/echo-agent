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
        assert sandbox.check_permission("network") is True

    def test_legacy_plugin_compat_allows_only_default_set(self):
        manifest = _make_manifest([])
        sandbox = PluginSandbox("test", manifest, trusted=False, mode="compat")
        assert sandbox.is_legacy is True
        assert sandbox.check_tool_register() is True
        assert sandbox.check_hook_register() is True
        assert sandbox.check_permission("network") is False
        assert sandbox.check_permission("subprocess") is False
        assert sandbox.check_permission("filesystem.write") is False

    def test_legacy_plugin_strict_allows_nothing(self):
        manifest = _make_manifest([])
        sandbox = PluginSandbox("test", manifest, trusted=False, mode="strict")
        assert sandbox.is_legacy is True
        assert sandbox.check_tool_register() is False
        assert sandbox.check_hook_register() is False
        assert sandbox.check_permission("network") is False

    def test_declared_permissions_unaffected_by_mode(self):
        manifest = _make_manifest(["network"])
        compat = PluginSandbox("t", manifest, trusted=False, mode="compat")
        strict = PluginSandbox("t", manifest, trusted=False, mode="strict")
        assert compat.check_permission("network") is True
        assert strict.check_permission("network") is True
        assert compat.check_tool_register() is False
        assert strict.check_tool_register() is False

    def test_trusted_bypasses_mode(self):
        manifest = _make_manifest([])
        sandbox = PluginSandbox("t", manifest, trusted=True, mode="strict")
        assert sandbox.check_permission("network") is True
        assert sandbox.check_tool_register() is True

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
        assert sandbox.check_permission("network") is False

    def test_violations_tracked(self):
        manifest = _make_manifest(["tool.register"])
        sandbox = PluginSandbox("test", manifest, trusted=False)
        sandbox.check_permission("network")
        sandbox.check_permission("subprocess")
        assert len(sandbox.violations) == 2
        assert "network" in sandbox.violations
        assert "subprocess" in sandbox.violations

    def test_filesystem_permissions(self):
        manifest = _make_manifest(["filesystem.read"])
        sandbox = PluginSandbox("test", manifest, trusted=False)
        assert sandbox.check_permission("filesystem.read") is True
        assert sandbox.check_permission("filesystem.write") is False


def test_plugins_config_default_permission_mode():
    from echo_agent.config.schema import PluginsConfig

    cfg = PluginsConfig()
    assert cfg.permission_mode == "compat"


def test_plugins_config_accepts_strict_mode():
    from echo_agent.config.schema import PluginsConfig

    cfg = PluginsConfig(permission_mode="strict")
    assert cfg.permission_mode == "strict"
