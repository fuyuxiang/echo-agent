"""Echo Agent Plugin System.

Public API for plugin authors and the agent core.
"""

from echo_agent.plugins.context import PluginContext
from echo_agent.plugins.errors import (
    PluginActivationError,
    PluginError,
    PluginLoadError,
    PluginManifestError,
)
from echo_agent.plugins.hooks import HookRegistry, HookResult, VALID_HOOKS
from echo_agent.plugins.manager import PluginManager
from echo_agent.plugins.manifest import PluginManifest, PluginProvides, PluginRecord

__all__ = [
    "PluginContext",
    "PluginManager",
    "PluginManifest",
    "PluginProvides",
    "PluginRecord",
    "HookRegistry",
    "HookResult",
    "VALID_HOOKS",
    "PluginError",
    "PluginLoadError",
    "PluginActivationError",
    "PluginManifestError",
]
