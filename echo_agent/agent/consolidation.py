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
            # Phase 1 (locked, fast): snapshot the unconsolidated chunk.
            # The session lock must NOT be held across the LLM calls below —
            # consolidation can take many seconds (summary + fact extraction +
            # contradiction checks), and the user's next message blocks on
            # this same lock for the entire duration.
            session_lock = await self._sessions.acquire(session_key)
            async with session_lock:
                session = await self._sessions.get_or_create(session_key)
                start = session.last_consolidated
                chunk = [dict(m) for m in session.messages[start:]]
                if not chunk:
                    return

            # Align the boundary on the snapshot: never consolidate up to a
            # trailing tool-call chain (its results may still be pending).
            boundary = len(chunk)
            while boundary > 0:
                msg = chunk[boundary - 1]
                if msg.get("role") == "tool":
                    boundary -= 1
                elif msg.get("role") == "assistant" and msg.get("tool_calls"):
                    boundary -= 1
                else:
                    break
            if boundary <= 0:
                return
            # Consolidate only up to the boundary — the trimmed tail gets
            # picked up next round, instead of being consolidated twice
            # (once now, once after the boundary rollback).
            trimmed = chunk[:boundary]

            # Phase 2 (unlocked, slow): LLM work on the immutable snapshot.
            chunk_ok = await self._consolidator.consolidate_chunk(trimmed)

            # Phase 3 (locked, fast): commit the new boundary — only if the
            # session region we consolidated is still intact (compression may
            # have rewritten history while the LLM ran).
            if chunk_ok:
                session_lock = await self._sessions.acquire(session_key)
                async with session_lock:
                    session = await self._sessions.get_or_create(session_key)
                    if self._snapshot_still_valid(session, start, chunk, boundary):
                        session.last_consolidated = start + boundary
                        await self._sessions.save(session)
                    else:
                        logger.info(
                            "Consolidation boundary for {} skipped: history changed during LLM work",
                            session_key,
                        )

            # Sleep consolidation works purely on the snapshot and the memory
            # store (which has its own locking) — no session lock needed.
            if self._sleep_consolidation:
                try:
                    stats = await self._consolidator.sleep_consolidate(
                        session_key, trimmed, chunk_already_consolidated=chunk_ok,
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

    @staticmethod
    def _snapshot_still_valid(session: Any, start: int, chunk: list[dict], boundary: int) -> bool:
        """The consolidated region must still match the snapshot before we
        advance the boundary over it. Full-region comparison: a partial check
        could be fooled by compression rewriting history into a tail that
        happens to end with an identical message."""
        if session.last_consolidated != start:
            return False
        if len(session.messages) < start + boundary:
            return False
        for offset in range(boundary):
            snap = chunk[offset]
            live = session.messages[start + offset]
            if live.get("role") != snap.get("role") or live.get("content") != snap.get("content"):
                return False
        return True
