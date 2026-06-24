"""Plugin sandbox — declaration tracking for plugin permissions.

trusted-operator threat model: plugins run in-process and are trusted. This
class is NOT a security boundary. tool.register / hook.register are enforced
at registration time (plugins/manager.py). network / subprocess / filesystem.*
are advisory manifest metadata only — they document intent and are surfaced
via check_permission() for introspection, but are NOT enforced at runtime
(in-process Python cannot constrain a plugin that does not voluntarily ask).
"""

from __future__ import annotations

from typing import TYPE_CHECKING, Literal

from loguru import logger

if TYPE_CHECKING:
    from echo_agent.plugins.manifest import PluginManifest

# network / subprocess / filesystem.* are advisory (declaration-only), NOT
# runtime-enforced. tool.register / hook.register are enforced at registration.
VALID_PERMISSIONS = frozenset({
    "filesystem.read",
    "filesystem.write",
    "network",
    "subprocess",
    "tool.register",
    "hook.register",
})

# legacy（未声明 permissions）插件在 compat 模式下获得的最小默认权限集。
# 仅含最常见、最低风险的注册类权限；network/subprocess/filesystem.* 须显式声明。
DEFAULT_LEGACY_PERMISSIONS = frozenset({
    "tool.register",
    "hook.register",
})


class PluginSandbox:
    """Enforces declared permissions for a plugin."""

    def __init__(
        self,
        plugin_name: str,
        manifest: "PluginManifest",
        *,
        trusted: bool = False,
        mode: Literal["compat", "strict"] = "compat",
    ):
        self._plugin_name = plugin_name
        self._trusted = trusted
        self._mode = mode
        self._is_legacy = len(manifest.permissions) == 0
        self._violations: list[str] = []
        self._effective_permissions: set[str]

        if self._is_legacy:
            self._effective_permissions = (
                set(DEFAULT_LEGACY_PERMISSIONS) if self._mode == "compat" else set()
            )
        else:
            self._effective_permissions = set(manifest.permissions)

        if self._is_legacy and not trusted:
            logger.warning(
                "Plugin '{}' has no permissions declared (legacy mode, "
                "permission_mode={}) — consider adding a 'permissions' field "
                "to its manifest",
                plugin_name,
                self._mode,
            )

    @property
    def is_legacy(self) -> bool:
        return self._is_legacy

    @property
    def violations(self) -> list[str]:
        return list(self._violations)

    def check_permission(self, required: str) -> bool:
        """Check if the plugin has the required permission.

        Returns True if allowed, False if denied.
        Trusted plugins always pass. Legacy plugins are governed by
        permission_mode (compat: default set; strict: nothing).
        """
        if self._trusted:
            return True

        if required in self._effective_permissions:
            return True

        self._violations.append(required)
        logger.warning(
            "Plugin '{}' attempted undeclared operation '{}' — blocked. "
            "Declare it in manifest permissions to allow.",
            self._plugin_name,
            required,
        )
        return False

    def check_tool_register(self) -> bool:
        return self.check_permission("tool.register")

    def check_hook_register(self) -> bool:
        return self.check_permission("hook.register")
