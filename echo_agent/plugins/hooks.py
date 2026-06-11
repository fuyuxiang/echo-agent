"""Hook registry — lifecycle hook dispatch for the plugin system."""

from __future__ import annotations

import inspect
from collections import defaultdict
from dataclasses import dataclass
from typing import Any, Callable, Coroutine

from loguru import logger


HookCallback = Callable[..., Coroutine[Any, Any, "HookResult | None"]]

VALID_HOOKS: frozenset[str] = frozenset({
    "on_agent_start",
    "on_agent_stop",
    "on_session_start",
    "on_session_end",
    "pre_tool_call",
    "post_tool_call",
    "pre_llm_call",
    "post_llm_call",
    "pre_approval",
    "post_approval",
    "on_error",
})


@dataclass
class HookResult:
    """Return value from a hook callback."""

    modified: Any = None
    stop_propagation: bool = False
    cancel: bool = False
    cancel_reason: str = ""


def _ensure_async(fn: Callable) -> HookCallback:
    """Wrap a sync function as async if needed."""
    if inspect.iscoroutinefunction(fn):
        return fn

    async def _wrapper(*args: Any, **kwargs: Any) -> HookResult | None:
        return fn(*args, **kwargs)

    _wrapper.__name__ = getattr(fn, "__name__", "anonymous")
    _wrapper.__qualname__ = getattr(fn, "__qualname__", "anonymous")
    return _wrapper


class HookRegistry:
    """Manages hook subscriptions and dispatches events.

    Semantics:
    - Fail-open: exceptions in callbacks are logged, never raised.
    - Ordered: hooks fire in registration order.
    - Async-first: all callbacks are awaited.
    """

    def __init__(self) -> None:
        self._hooks: dict[str, list[tuple[str, HookCallback]]] = defaultdict(list)

    def register(self, hook_name: str, callback: Callable, *, plugin: str = "") -> None:
        """Register a callback for a lifecycle hook."""
        if hook_name not in VALID_HOOKS:
            logger.warning(
                "Plugin '{}' registered unknown hook '{}' — it will still be stored",
                plugin, hook_name,
            )
        async_cb = _ensure_async(callback)
        self._hooks[hook_name].append((plugin, async_cb))

    def unregister_plugin(self, plugin_name: str) -> None:
        """Remove all hooks registered by a specific plugin."""
        for hook_name in list(self._hooks.keys()):
            self._hooks[hook_name] = [
                (p, cb) for p, cb in self._hooks[hook_name] if p != plugin_name
            ]

    async def dispatch(self, hook_name: str, *args: Any, **kwargs: Any) -> list[HookResult]:
        """Invoke all callbacks for a hook. Returns list of non-None results."""
        results: list[HookResult] = []
        for plugin_name, callback in self._hooks.get(hook_name, []):
            try:
                result = await callback(*args, **kwargs)
                if result is not None:
                    results.append(result)
                    if result.stop_propagation:
                        break
            except Exception as e:
                logger.warning(
                    "Plugin '{}' hook '{}' raised {}: {}",
                    plugin_name, hook_name, type(e).__name__, e,
                )
        return results

    async def dispatch_modify(self, hook_name: str, value: Any, *args: Any, **kwargs: Any) -> Any:
        """Dispatch hooks that can modify a value. Returns the (possibly modified) value."""
        for plugin_name, callback in self._hooks.get(hook_name, []):
            try:
                result = await callback(value, *args, **kwargs)
                if result is not None and result.modified is not None:
                    value = result.modified
                if result is not None and result.stop_propagation:
                    break
            except Exception as e:
                logger.warning(
                    "Plugin '{}' hook '{}' raised {}: {}",
                    plugin_name, hook_name, type(e).__name__, e,
                )
        return value

    def has_hooks(self, hook_name: str) -> bool:
        """Check if any callbacks are registered for a hook."""
        return bool(self._hooks.get(hook_name))

    def get_registered_hooks(self) -> dict[str, list[str]]:
        """Return {hook_name: [plugin_names]} for introspection."""
        return {
            name: [p for p, _ in callbacks]
            for name, callbacks in self._hooks.items()
            if callbacks
        }
