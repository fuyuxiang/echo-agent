"""Consolidation worker — safely consolidates session history in the background.

Fixes the race condition where the old approach passed a mutable session object
to a background task. This worker re-acquires the session lock and reloads
the session, ensuring no concurrent mutation.
"""

from __future__ import annotations

import asyncio
from collections.abc import Callable, Coroutine
from typing import Any

from loguru import logger

from echo_agent.memory.consolidator import MemoryConsolidator
from echo_agent.session.manager import SessionManager


class ConsolidationWorker:
    """Schedules and runs session consolidation safely."""

    def __init__(
        self,
        sessions: SessionManager,
        consolidator: MemoryConsolidator,
        *,
        sleep_consolidation: bool = False,
    ):
        self._sessions = sessions
        self._consolidator = consolidator
        self._sleep_consolidation = sleep_consolidation
        self._pending: set[str] = set()
        self._lock = asyncio.Lock()

    async def schedule(
        self,
        session_key: str,
        spawn_fn: Callable[[Coroutine[Any, Any, None]], None],
        on_complete: Callable[[str], Coroutine[Any, Any, None]] | None = None,
    ) -> None:
        async with self._lock:
            if session_key in self._pending:
                return
            self._pending.add(session_key)
        spawn_fn(self._run(session_key, on_complete))

    def is_pending(self, session_key: str) -> bool:
        return session_key in self._pending

    async def _run(
        self,
        session_key: str,
        on_complete: Callable[[str], Coroutine[Any, Any, None]] | None = None,
    ) -> None:
        try:
            session_lock = await self._sessions.acquire(session_key)
            async with session_lock:
                session = await self._sessions.get_or_create(session_key)
                chunk = list(session.messages[session.last_consolidated:])
                if not chunk:
                    return

                chunk_ok = await self._consolidator.consolidate_chunk(chunk)
                if chunk_ok:
                    boundary = len(session.messages)
                    while boundary > session.last_consolidated:
                        msg = session.messages[boundary - 1]
                        if msg.get("role") == "tool":
                            boundary -= 1
                        elif msg.get("role") == "assistant" and msg.get("tool_calls"):
                            boundary -= 1
                        else:
                            break
                    if boundary > session.last_consolidated:
                        session.last_consolidated = boundary
                        await self._sessions.save(session)

                if self._sleep_consolidation:
                    try:
                        stats = await self._consolidator.sleep_consolidate(
                            session_key, chunk, chunk_already_consolidated=chunk_ok,
                        )
                        if any(v > 0 for v in stats.values()):
                            logger.info("Sleep consolidation for {}: {}", session_key, stats)
                    except Exception as e:
                        logger.warning("Sleep consolidation failed: {}", e)

            if on_complete:
                await on_complete(session_key)

        except Exception as e:
            logger.error("Consolidation failed for {}: {}", session_key, e)
        finally:
            async with self._lock:
                self._pending.discard(session_key)
