"""Plugin manager — orchestrates the full plugin lifecycle."""

from __future__ import annotations

import inspect
from pathlib import Path
from typing import Any, TYPE_CHECKING

from loguru import logger

from echo_agent.plugins.context import PluginContext
from echo_agent.plugins.hooks import HookRegistry
from echo_agent.plugins.loader import (
    discover_all,
    load_plugin_module,
    topological_sort,
)
from echo_agent.plugins.manifest import PluginRecord, check_required_env
from echo_agent.plugins.sandbox import PluginSandbox

if TYPE_CHECKING:
    from echo_agent.agent.tools.registry import ToolRegistry
    from echo_agent.bus.queue import MessageBus
    from echo_agent.config.schema import Config
    from echo_agent.models.provider import LLMProvider


class PluginManager:
    """Orchestrates plugin discovery, loading, activation, and shutdown."""

    def __init__(
        self,
        *,
        config: "Config",
        workspace: Path,
        bus: "MessageBus",
        tool_registry: "ToolRegistry",
        provider: "LLMProvider | None" = None,
    ) -> None:
        self._config = config
        self._workspace = workspace
        self._bus = bus
        self._tool_registry = tool_registry
        self._provider = provider
        self._hooks = HookRegistry()
        self._plugins: list[PluginRecord] = []
        self._contexts: dict[str, PluginContext] = {}
        self._deactivators: dict[str, Any] = {}

    @property
    def hooks(self) -> HookRegistry:
        return self._hooks

    @property
    def plugins(self) -> list[PluginRecord]:
        return list(self._plugins)

    async def discover_and_load(self) -> None:
        """Discover, filter, sort, and activate all plugins."""
        plugins_cfg = self._config.plugins
        if not plugins_cfg.enabled:
            logger.info("Plugin system disabled by config")
            return

        records = discover_all(
            workspace=self._workspace,
            extra_dirs=plugins_cfg.extra_dirs,
        )

        if not records:
            logger.debug("No plugins discovered")
            return

        records = self._filter_plugins(records, plugins_cfg)
        records = topological_sort(records)

        logger.info("Discovered {} plugin(s), loading...", len(records))

        for record in records:
            await self._load_and_activate(record)

        self._plugins = records

        loaded = [r for r in records if r.status == "activated"]
        failed = [r for r in records if r.status == "failed"]
        if loaded:
            logger.info(
                "Loaded {} plugin(s): {}",
                len(loaded),
                ", ".join(r.manifest.name for r in loaded),
            )
        if failed:
            logger.warning(
                "{} plugin(s) failed to load: {}",
                len(failed),
                ", ".join(f"{r.manifest.name} ({r.error})" for r in failed),
            )

    def _filter_plugins(self, records: list[PluginRecord], plugins_cfg: Any) -> list[PluginRecord]:
        """Apply allow/deny lists from config."""
        deny_set = set(plugins_cfg.deny)
        allow_set = set(plugins_cfg.allow)

        filtered: list[PluginRecord] = []
        for record in records:
            name = record.manifest.name
            if name in deny_set:
                record.status = "disabled"
                logger.debug("Plugin '{}' disabled by deny list", name)
                continue
            if allow_set and name not in allow_set:
                record.status = "disabled"
                logger.debug("Plugin '{}' not in allow list, skipping", name)
                continue
            filtered.append(record)
        return filtered

    async def _load_and_activate(self, record: PluginRecord) -> None:
        """Load module and call activate() for a single plugin."""
        name = record.manifest.name

        missing_env = check_required_env(record.manifest)
        if missing_env:
            record.status = "failed"
            record.error = f"Missing env vars: {', '.join(missing_env)}"
            logger.warning("Plugin '{}' skipped: {}", name, record.error)
            return

        try:
            interface = load_plugin_module(record)
        # 第三方插件的导入副作用可能抛任何异常,一律记为加载失败而非拖垮宿主。
        # 原写法 (PluginLoadError, Exception) 里 PluginLoadError 已被 Exception 覆盖。
        except Exception as e:
            record.status = "failed"
            record.error = str(e)
            logger.warning("Plugin '{}' failed to load: {}", name, e)
            return

        record.status = "loaded"

        plugin_config = self._config.plugins.config.get(
            record.manifest.config_key or name, {}
        )

        trusted_list = getattr(self._config.plugins, "trusted_plugins", []) or []
        is_trusted = name in trusted_list
        mode = getattr(self._config.plugins, "permission_mode", "compat")
        sandbox = PluginSandbox(name, record.manifest, trusted=is_trusted, mode=mode)

        ctx = PluginContext(
            plugin_name=name,
            config=self._config,
            workspace=self._workspace,
            bus=self._bus,
            tool_registry=self._tool_registry,
            hook_registry=self._hooks,
            provider=self._provider,
            plugin_config=plugin_config,
        )

        # activate 前权限预检：strict 模式下越权拒绝加载（不调 activate）；
        # compat 模式越权仅 warning 并继续，靠事后核验裁剪；legacy 插件按默认权限集放行。
        # 权限判定在 sandbox 内不随时间变化，只调一次并缓存，事后核验复用，
        # 避免重复 append violations 与重复打印 warning。
        tool_ok = sandbox.check_tool_register() if record.manifest.provides.tools else True
        hook_ok = sandbox.check_hook_register() if record.manifest.provides.hooks else True
        denied: list[str] = []
        if record.manifest.provides.tools and not tool_ok:
            denied.append("tool.register")
        if record.manifest.provides.hooks and not hook_ok:
            denied.append("hook.register")

        if denied and mode == "strict":
            record.status = "failed"
            record.error = f"permission denied for: {', '.join(denied)}"
            logger.warning(
                "Plugin '{}' rejected before activate: {}", name, record.error
            )
            return

        activate_fn = interface["activate"]
        deactivate_fn = interface.get("deactivate")

        try:
            if inspect.iscoroutinefunction(activate_fn):
                await activate_fn(ctx)
            else:
                activate_fn(ctx)
        except Exception as e:
            record.status = "failed"
            record.error = f"activate() raised: {e}"
            logger.warning("Plugin '{}' activation failed: {}", name, e)
            self._hooks.unregister_plugin(name)
            return

        if ctx.registered_tools and not tool_ok:
            logger.warning("Plugin '{}' registered tools without permission — unregistering", name)
            for tool_name in ctx.registered_tools:
                self._tool_registry.unregister(tool_name)
            ctx._registered_tools.clear()

        if ctx.registered_hooks and not hook_ok:
            logger.warning("Plugin '{}' registered hooks without permission — unregistering", name)
            self._hooks.unregister_plugin(name)
            ctx._registered_hooks.clear()

        record.status = "activated"
        record.tools_registered = ctx.registered_tools
        record.hooks_registered = ctx.registered_hooks
        self._contexts[name] = ctx

        if deactivate_fn is not None:
            self._deactivators[name] = deactivate_fn

    async def shutdown(self) -> None:
        """Call deactivate() on all activated plugins."""
        for name, deactivate_fn in self._deactivators.items():
            ctx = self._contexts.get(name)
            if ctx is None:
                continue
            try:
                if inspect.iscoroutinefunction(deactivate_fn):
                    await deactivate_fn(ctx)
                else:
                    deactivate_fn(ctx)
            except Exception as e:
                logger.warning("Plugin '{}' deactivate() raised: {}", name, e)

        self._deactivators.clear()
        self._contexts.clear()

    def get_plugin_info(self, name: str) -> PluginRecord | None:
        """Get a plugin record by name."""
        for record in self._plugins:
            if record.manifest.name == name:
                return record
        return None

    def get_status_report(self) -> list[dict[str, Any]]:
        """Return a summary of all discovered plugins."""
        return [
            {
                "name": r.manifest.name,
                "version": r.manifest.version,
                "description": r.manifest.description,
                "status": r.status,
                "source": r.source,
                "error": r.error,
                "tools": r.tools_registered,
                "hooks": r.hooks_registered,
            }
            for r in self._plugins
        ]
