"""Plugin manager — orchestrates the full plugin lifecycle."""

from __future__ import annotations

import asyncio
import inspect
from pathlib import Path
from typing import Any, TYPE_CHECKING

from loguru import logger

from echo_agent.plugins.context import PluginContext
from echo_agent.plugins.errors import PluginPermissionError
from echo_agent.plugins.hooks import HookRegistry
from echo_agent.plugins.loader import (
    discover_all,
    load_plugin_module,
    topological_sort,
)
from echo_agent.plugins.manifest import PluginRecord, check_required_env
from echo_agent.plugins.sandbox import PluginSandbox, VALID_PERMISSIONS

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
        self._owned_tools: dict[str, list[Any]] = {}
        self._shutdown_lock = asyncio.Lock()

    @property
    def hooks(self) -> HookRegistry:
        return self._hooks

    @property
    def plugins(self) -> list[PluginRecord]:
        return list(self._plugins)

    @property
    def owned_tool_identities(self) -> frozenset[int]:
        """Read-only identity snapshot of tools currently owned by plugins."""
        return frozenset(
            id(tool)
            for tools in self._owned_tools.values()
            for tool in tools
        )

    def owns_tool(self, tool: Any) -> bool:
        """Return whether this manager owns this exact object, never just its name."""
        return any(
            owned is tool
            for tools in self._owned_tools.values()
            for owned in tools
        )

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
        self._plugins = records

        logger.info("Discovered {} plugin(s), loading...", len(records))

        records_by_name = {record.manifest.name: record for record in records}
        for record in records:
            if record.status == "failed":
                continue
            unavailable = [
                dependency
                for dependency in record.manifest.depends_on
                if records_by_name[dependency].status != "activated"
            ]
            if unavailable:
                record.status = "failed"
                record.error = (
                    "dependencies not activated: " + ", ".join(sorted(unavailable))
                )
                logger.warning("Plugin '{}' skipped: {}", record.manifest.name, record.error)
                continue
            await self._load_and_activate(record)

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

        unknown_permissions = sorted(
            set(record.manifest.permissions) - VALID_PERMISSIONS
        )
        if unknown_permissions:
            record.status = "failed"
            record.error = f"unknown permissions: {', '.join(unknown_permissions)}"
            logger.warning("Plugin '{}' skipped: {}", name, record.error)
            return

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

        # Declared provides allow strict mode to reject before executing plugin
        # code.  They are only a hint, never the authority: actual registrations
        # are checked again below so omitting/falsifying provides cannot bypass
        # tool.register or hook.register.
        tool_ok = (
            sandbox.check_tool_register() if record.manifest.provides.tools else None
        )
        hook_ok = (
            sandbox.check_hook_register() if record.manifest.provides.hooks else None
        )
        denied: list[str] = []
        if tool_ok is False:
            denied.append("tool.register")
        if hook_ok is False:
            denied.append("hook.register")

        if denied and mode == "strict":
            record.status = "failed"
            record.error = f"permission denied for: {', '.join(denied)}"
            logger.warning(
                "Plugin '{}' rejected before activate: {}", name, record.error
            )
            return

        def allow_tool_registration() -> bool:
            nonlocal tool_ok
            if tool_ok is None:
                tool_ok = sandbox.check_tool_register()
            return tool_ok

        def allow_hook_registration() -> bool:
            nonlocal hook_ok
            if hook_ok is None:
                hook_ok = sandbox.check_hook_register()
            return hook_ok

        ctx = PluginContext(
            plugin_name=name,
            config=self._config,
            workspace=self._workspace,
            bus=self._bus,
            tool_registry=self._tool_registry,
            hook_registry=self._hooks,
            provider=self._provider,
            plugin_config=plugin_config,
            tool_registration_allowed=allow_tool_registration,
            hook_registration_allowed=allow_hook_registration,
            registration_mode=mode,
        )

        activate_fn = interface["activate"]
        deactivate_fn = interface.get("deactivate")

        try:
            result = activate_fn(ctx)
            if inspect.isawaitable(result):
                await result
        except asyncio.CancelledError as cancellation:
            record.status = "failed"
            record.error = "activate() cancelled"
            # Activation may already have published tools/hooks/subscriptions.
            # Roll them back to completion before preserving cancellation.
            try:
                await self._release_context_resources_shielded(name, ctx)
            except asyncio.CancelledError:
                # A repeated cancel is remembered by the shield helper, but the
                # original cancellation remains the public activation outcome.
                pass
            raise cancellation
        except PluginPermissionError as e:
            record.status = "failed"
            record.error = str(e)
            logger.warning("Plugin '{}' activation denied: {}", name, e)
            await self._release_context_resources_shielded(name, ctx)
            return
        except Exception as e:
            record.status = "failed"
            record.error = f"activate() raised: {e}"
            logger.warning("Plugin '{}' activation failed: {}", name, e)
            await self._release_context_resources_shielded(name, ctx)
            return

        actual_denied = sorted(ctx.denied_registrations)

        if actual_denied and mode == "strict":
            record.status = "failed"
            record.error = f"permission denied for actual registrations: {', '.join(actual_denied)}"
            logger.warning("Plugin '{}' activation rolled back: {}", name, record.error)
            await self._release_context_resources_shielded(name, ctx)
            return

        if actual_denied:
            logger.warning(
                "Plugin '{}' attempted registrations without permission: {}",
                name,
                ", ".join(actual_denied),
            )
            denied_tools = ctx.denied_tool_instances
            cleanup_cancellation: asyncio.CancelledError | None = None
            try:
                await self._await_cleanup_shielded(
                    self._close_tools(name, denied_tools)
                )
            except asyncio.CancelledError as cancellation:
                cleanup_cancellation = cancellation
            finally:
                # The shield waits until every close attempt is terminal before
                # propagating cancellation, so these objects must not be offered
                # to the subsequent full rollback a second time.
                ctx._denied_tool_instances.clear()
                ctx._denied_registrations.clear()
            if cleanup_cancellation is not None:
                # Activation was cancelled during compatibility cleanup. Roll
                # back admitted resources as well before handing cancellation
                # to the caller.
                try:
                    await self._release_context_resources_shielded(name, ctx)
                except asyncio.CancelledError:
                    # Cleanup has already converged; preserve the earlier
                    # cancellation object as the public activation outcome.
                    pass
                raise cleanup_cancellation

        record.status = "activated"
        record.tools_registered = ctx.registered_tools
        record.hooks_registered = ctx.registered_hooks
        self._contexts[name] = ctx
        self._owned_tools[name] = ctx.registered_tool_instances

        if deactivate_fn is not None:
            self._deactivators[name] = deactivate_fn

    async def shutdown(self) -> None:
        """Deactivate plugins and close their tools in reverse dependency order.

        ``discover_and_load`` stores records in dependency-first topological
        order.  Dependents therefore unwind first.  A plugin's deactivate hook
        runs while its own tools and every dependency are still usable; its
        tools are then closed by the manager even when the hook is absent or
        fails.  Ownership is consumed before callbacks run, making repeated or
        concurrent shutdown calls idempotent for non-idempotent third-party
        tools.
        """
        async with self._shutdown_lock:
            ordered_names = [
                record.manifest.name
                for record in reversed(self._plugins)
                if record.manifest.name in self._contexts
            ]
            # Direct _load_and_activate() callers (including embedders/tests)
            # have contexts without a discover_and_load() record list. Preserve
            # their activation order and still unwind it LIFO.
            ordered_names.extend(
                name for name in reversed(self._contexts)
                if name not in ordered_names
            )

            cancellation: asyncio.CancelledError | None = None
            for name in ordered_names:
                # Pop ownership up front. If a callback re-enters shutdown, or
                # the caller retries after a failure, this plugin cannot be
                # deactivated/closed twice.
                ctx = self._contexts.pop(name, None)
                deactivate_fn = self._deactivators.pop(name, None)
                tools = self._owned_tools.pop(name, [])
                if ctx is None:
                    continue

                if deactivate_fn is not None:
                    try:
                        result = deactivate_fn(ctx)
                        if inspect.isawaitable(result):
                            await result
                    except asyncio.CancelledError as e:
                        # Finish releasing owned resources, then preserve the
                        # caller's cancellation at the public boundary.
                        cancellation = cancellation or e
                    except Exception as e:
                        logger.warning("Plugin '{}' deactivate() raised: {}", name, e)

                try:
                    await self._release_context_resources_shielded(
                        name, ctx, tools=tools,
                    )
                except asyncio.CancelledError as e:  # pragma: no cover - defensive
                    cancellation = cancellation or e

            # Drop any stale bookkeeping left by a partially constructed plugin
            # manager. There is no lifecycle owner behind those entries now.
            self._deactivators.clear()
            self._contexts.clear()
            self._owned_tools.clear()
            if cancellation is not None:
                raise cancellation

    async def _release_context_resources(
        self,
        plugin_name: str,
        ctx: PluginContext,
        *,
        tools: list[Any] | None = None,
    ) -> None:
        """Withdraw every resource registered through a PluginContext."""
        for handler in reversed(ctx.registered_inbound_handlers):
            try:
                self._bus.unsubscribe_inbound(handler)
            except Exception as e:
                logger.warning(
                    "Plugin '{}' inbound unsubscribe raised: {}", plugin_name, e,
                )
        ctx._registered_inbound_handlers.clear()
        try:
            self._hooks.unregister_plugin(plugin_name)
        except Exception as e:  # pragma: no cover - built-in registry is total
            logger.warning("Plugin '{}' hook unregister raised: {}", plugin_name, e)
        finally:
            ctx._registered_hooks.clear()
            try:
                await self._close_tools(
                    plugin_name,
                    ctx.owned_tool_instances if tools is None else tools,
                )
            finally:
                ctx._registered_tools.clear()
                ctx._registered_tool_instances.clear()
                ctx._denied_tool_instances.clear()
                ctx._denied_registrations.clear()

    async def _release_context_resources_shielded(
        self,
        plugin_name: str,
        ctx: PluginContext,
        *,
        tools: list[Any] | None = None,
    ) -> None:
        """Finish activation rollback even if its owner is cancelled again."""
        await self._await_cleanup_shielded(
            self._release_context_resources(plugin_name, ctx, tools=tools)
        )

    @staticmethod
    async def _await_cleanup_shielded(cleanup_awaitable: Any) -> None:
        """Converge one cleanup task before surfacing repeated cancellation."""
        cleanup = asyncio.ensure_future(cleanup_awaitable)
        repeated_cancellation: asyncio.CancelledError | None = None
        while not cleanup.done():
            try:
                await asyncio.shield(cleanup)
            except asyncio.CancelledError as e:
                # shield keeps cleanup alive. Keep waiting through any number
                # of cancel() calls, then propagate the newest cancellation.
                repeated_cancellation = e
            except BaseException:
                # The cleanup task owns this exception; retrieve it below so it
                # is observed exactly once with its original traceback.
                break
        cleanup.result()
        if repeated_cancellation is not None:
            raise repeated_cancellation

    async def _close_tools(self, plugin_name: str, tools: list[Any]) -> None:
        """Best-effort close and unregister exact plugin-owned tool instances."""
        seen: set[int] = set()
        cancellation: asyncio.CancelledError | None = None
        for tool in reversed(tools):
            identity = id(tool)
            if identity in seen:
                continue
            seen.add(identity)
            tool_name = str(getattr(tool, "name", ""))
            close = getattr(tool, "aclose", None)
            if callable(close):
                try:
                    result = close()
                    if inspect.isawaitable(result):
                        await result
                except asyncio.CancelledError as e:
                    cancellation = cancellation or e
                except Exception as e:
                    logger.warning(
                        "Plugin '{}' tool '{}' aclose() raised: {}",
                        plugin_name, tool_name or type(tool).__name__, e,
                    )
            # Do not unregister a replacement installed by another lifecycle
            # owner after activation. The captured instance is still closed.
            if tool_name:
                try:
                    if self._tool_registry.get(tool_name) is tool:
                        self._tool_registry.unregister(tool_name)
                except Exception as e:  # pragma: no cover - defensive adapter seam
                    logger.warning(
                        "Plugin '{}' tool '{}' unregister raised: {}",
                        plugin_name, tool_name, e,
                    )
        if cancellation is not None:
            raise cancellation

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
