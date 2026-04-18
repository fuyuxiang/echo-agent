"""Event hook system for gateway extensibility."""

from __future__ import annotations

import asyncio
import importlib.util
from collections import defaultdict
from pathlib import Path
from typing import Any, Callable, Awaitable

from loguru import logger

HookHandler = Callable[..., Awaitable[None]]


class HookRegistry:

    EVENTS = frozenset({
        "message_received",
        "message_sent",
        "session_reset",
        "auth_success",
        "auth_failed",
        "compression_done",
        "gateway_start",
        "gateway_stop",
    })

    def __init__(self) -> None:
        self._handlers: dict[str, list[HookHandler]] = defaultdict(list)

    def register(self, event: str, handler: HookHandler) -> None:
        if event not in self.EVENTS:
            logger.warning("Unknown hook event: {}", event)
        self._handlers[event].append(handler)

    async def emit(self, event: str, **kwargs: Any) -> None:
        handlers = self._handlers.get(event, [])
        if not handlers:
            return

        results = await asyncio.gather(
            *(self._safe_call(h, event, kwargs) for h in handlers),
            return_exceptions=True,
        )
        for r in results:
            if isinstance(r, Exception):
                logger.error("Hook handler error for {}: {}", event, r)

    async def _safe_call(
        self, handler: HookHandler, event: str, kwargs: dict[str, Any],
    ) -> None:
        try:
            await handler(**kwargs)
        except Exception as e:
            logger.error("Hook {} failed: {}", event, e)
            raise

    def load_from_dir(self, hooks_dir: Path) -> int:
        if not hooks_dir.is_dir():
            return 0

        loaded = 0
        for path in sorted(hooks_dir.glob("*.py")):
            try:
                spec = importlib.util.spec_from_file_location(
                    f"echo_hook_{path.stem}", path,
                )
                if spec is None or spec.loader is None:
                    continue
                module = importlib.util.module_from_spec(spec)
                spec.loader.exec_module(module)

                register_fn = getattr(module, "register_hooks", None)
                if callable(register_fn):
                    register_fn(self)
                    loaded += 1
                    logger.info("Loaded hook module: {}", path.name)
            except Exception as e:
                logger.error("Failed to load hook {}: {}", path.name, e)

        return loaded

    @property
    def handler_count(self) -> int:
        return sum(len(v) for v in self._handlers.values())
