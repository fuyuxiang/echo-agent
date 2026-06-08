"""Plugin sandbox — permission-based access control for plugin operations.

Validates that plugins only perform operations they have declared in their
manifest permissions field. Untrusted plugins that attempt undeclared
operations get logged and blocked.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

from loguru import logger

if TYPE_CHECKING:
    from echo_agent.plugins.manifest import PluginManifest

VALID_PERMISSIONS = frozenset({
    "filesystem.read",
    "filesystem.write",
    "network",
    "subprocess",
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
    ):
        self._plugin_name = plugin_name
        self._permissions = set(manifest.permissions)
        self._trusted = trusted
        self._is_legacy = len(manifest.permissions) == 0
        self._violations: list[str] = []

        if self._is_legacy and not trusted:
            logger.warning(
                "Plugin '{}' has no permissions declared (legacy mode) — "
                "consider adding a 'permissions' field to its manifest",
                plugin_name,
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
        Legacy plugins (no permissions declared) are allowed but warned.
        Trusted plugins always pass.
        """
        if self._trusted:
            return True

        if self._is_legacy:
            return True

        if required in self._permissions:
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

    def check_network(self) -> bool:
        return self.check_permission("network")

    def check_filesystem_write(self) -> bool:
        return self.check_permission("filesystem.write")

    def check_filesystem_read(self) -> bool:
        return self.check_permission("filesystem.read")

    def check_subprocess(self) -> bool:
        return self.check_permission("subprocess")
