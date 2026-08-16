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


def _accepts_var_keyword(fn: Callable) -> bool:
    """Whether *fn* declares **kwargs (and can therefore take anything)."""
    try:
        params = inspect.signature(fn).parameters.values()
    except (ValueError, TypeError):
        # Builtins and some C callables have no introspectable signature.
        # Pass everything through — same behaviour as before this filter existed.
        return True
    return any(p.kind is inspect.Parameter.VAR_KEYWORD for p in params)


def _known_keywords(fn: Callable) -> set[str]:
    try:
        return {
            name
            for name, p in inspect.signature(fn).parameters.items()
            if p.kind
            in (inspect.Parameter.POSITIONAL_OR_KEYWORD, inspect.Parameter.KEYWORD_ONLY)
        }
    except (ValueError, TypeError):
        return set()


def _ensure_async(fn: Callable) -> HookCallback:
    """Wrap a callback so it is awaitable and tolerant of unknown kwargs.

    Keyword arguments a callback does not declare are dropped rather than
    forwarded. Without this, adding a new kwarg at a dispatch site raises
    TypeError inside every hook with a fixed signature; dispatch catches it and
    logs a warning, so the plugin silently stops working — worse than a crash
    because nothing obvious breaks. Filtering here lets the core add hook
    parameters without coordinating with every plugin author.

    Positional arguments are still passed as-is: adding or removing those
    remains a breaking change, so new hook inputs must be keyword arguments.
    """
    accepts_all = _accepts_var_keyword(fn)
    known = set() if accepts_all else _known_keywords(fn)

    async def _wrapper(*args: Any, **kwargs: Any) -> HookResult | None:
        if accepts_all:
            kw = kwargs
        else:
            kw = {k: v for k, v in kwargs.items() if k in known}
        result = fn(*args, **kw)
        if inspect.isawaitable(result):
            return await result
        return result

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
