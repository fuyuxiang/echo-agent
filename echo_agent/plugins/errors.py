"""Plugin system exceptions."""

from __future__ import annotations


class PluginError(Exception):
    """Base exception for plugin system errors."""

    def __init__(self, plugin_name: str, message: str):
        self.plugin_name = plugin_name
        super().__init__(f"[{plugin_name}] {message}")


class PluginLoadError(PluginError):
    """Raised when a plugin cannot be loaded (import failure, missing manifest, etc.)."""


class PluginActivationError(PluginError):
    """Raised when a plugin's activate() function fails."""


class PluginManifestError(PluginError):
    """Raised when a plugin manifest is invalid or missing required fields."""
