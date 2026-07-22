"""Bounded, TTL-based store for A2A tasks.

A2AProtocol used a plain unbounded dict for tasks, so completed tasks lived
forever and a long-running server leaked memory without limit. This store
keeps terminal tasks (completed/failed/canceled) around for a TTL window so
tasks/get can still fetch a recent result, then reclaims them. Active tasks
are never expired and never evicted for capacity — dropping a task mid-flight
would strand the caller. No persistence: on restart the store is empty, which
matches the existing in-memory semantics.
"""
from __future__ import annotations

import time
from collections import OrderedDict
from typing import Any, Callable

from echo_agent.a2a.models import A2ATask, TaskState

# Terminal states mirror A2AProtocol._TERMINAL_STATES. Only tasks in these
# states are subject to TTL expiry and capacity eviction.
_TERMINAL_STATES = frozenset({TaskState.COMPLETED, TaskState.FAILED, TaskState.CANCELED})


class TaskStore:
    """dict-like store: TTL-expire and capacity-bound terminal tasks only."""

    def __init__(
        self,
        *,
        ttl_seconds: float = 3600.0,
        max_tasks: int = 1000,
        clock: Callable[[], float] = time.monotonic,
    ) -> None:
        self._ttl = ttl_seconds
        self._max = max_tasks
        self._clock = clock
        # Insertion-ordered so the oldest terminal task is evicted first.
        self._tasks: "OrderedDict[str, A2ATask]" = OrderedDict()
        # task_id -> monotonic deadline; only present for terminal tasks.
        self._expire_at: dict[str, float] = {}

    # --- internal maintenance -------------------------------------------------

    def _purge_expired(self) -> None:
        """Drop terminal tasks whose TTL deadline has passed."""
        now = self._clock()
        expired = [tid for tid, deadline in self._expire_at.items() if now >= deadline]
        for tid in expired:
            self._tasks.pop(tid, None)
            self._expire_at.pop(tid, None)

    def _evict_for_capacity(self) -> None:
        """When over capacity, evict oldest terminal tasks; keep active ones."""
        while len(self._tasks) > self._max:
            victim = next(
                (tid for tid in self._tasks if tid in self._expire_at),
                None,
            )
            if victim is None:
                # Every task is active — refuse to drop live work, stay over cap.
                break
            self._tasks.pop(victim, None)
            self._expire_at.pop(victim, None)

    @staticmethod
    def _is_terminal(task: A2ATask) -> bool:
        return task.state in _TERMINAL_STATES

    # --- dict-like protocol ---------------------------------------------------

    def __setitem__(self, key: str, task: A2ATask) -> None:
        self._tasks[key] = task
        self._tasks.move_to_end(key)  # freshest at the end for LRU eviction
        if self._is_terminal(task):
            # (Re)arm TTL from now — covers both fresh terminal inserts and a
            # WORKING task that has just transitioned to a terminal state.
            self._expire_at[key] = self._clock() + self._ttl
        else:
            # Still active: immune to TTL until it becomes terminal.
            self._expire_at.pop(key, None)
        self._evict_for_capacity()

    def __getitem__(self, key: str) -> A2ATask:
        self._purge_expired()
        return self._tasks[key]

    def __contains__(self, key: str) -> bool:
        self._purge_expired()
        return key in self._tasks

    def __len__(self) -> int:
        self._purge_expired()
        return len(self._tasks)

    def get(self, key: str, default: Any = None) -> Any:
        self._purge_expired()
        return self._tasks.get(key, default)
