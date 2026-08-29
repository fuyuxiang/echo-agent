"""Bounded, TTL-based store for A2A tasks.

A2AProtocol used a plain unbounded dict for tasks, so completed tasks lived
forever and a long-running server leaked memory without limit. This store
keeps terminal tasks (completed/failed/canceled) around for a TTL window so
tasks/get can still fetch a recent result, then reclaims them. Active tasks are
immune to the normal result TTL and capacity eviction, but a generous active
backstop requests cancellation through the protocol so stuck work cannot live
forever. No persistence: on restart the store is empty, which matches the
existing in-memory semantics.
"""
from __future__ import annotations

import time
from collections import OrderedDict
from typing import Any, Callable

from echo_agent.a2a.models import A2ATask, TaskState

# The single definition of "no further transition allowed". Only tasks in these
# states are subject to TTL expiry and capacity eviction, and A2AProtocol imports
# this same frozenset to decide what may be cancelled — keeping one definition
# matters because the two would fail asymmetrically if they drifted: a state the
# protocol treats as terminal but the store does not would be immune to BOTH TTL
# and capacity eviction (an unbounded leak), while the reverse would reclaim
# in-flight tasks out from under their caller.
TERMINAL_STATES = frozenset({TaskState.COMPLETED, TaskState.FAILED, TaskState.CANCELED})
_TERMINAL_STATES = TERMINAL_STATES  # backward-compatible alias
DEFAULT_TASK_OWNER = "anonymous"


def _owned_storage_key(owner: str, task_id: str) -> str:
    """Return a collision-free internal key for an owner/task-id pair.

    The anonymous form stays unchanged for the long-standing dict-like public
    API and its embedders. Authenticated principals use a NUL-prefixed,
    length-delimited key. Protocol task IDs reject control characters, so an
    untrusted custom ID cannot manufacture one of these internal keys.
    """
    owner = owner or DEFAULT_TASK_OWNER
    if owner == DEFAULT_TASK_OWNER:
        return task_id
    return f"\0a2a:{len(owner)}:{owner}:{task_id}"


class TaskStore:
    """dict-like store: TTL-expire and capacity-bound terminal tasks only."""

    def __init__(
        self,
        *,
        ttl_seconds: float = 3600.0,
        max_tasks: int = 1000,
        active_ttl_seconds: float = 86400.0,
        clock: Callable[[], float] = time.monotonic,
        on_drop: Callable[[str], None] | None = None,
    ) -> None:
        # Fail fast rather than silently: a non-positive ttl or capacity made
        # every terminal task vanish the moment it was stored, so tasks/send
        # succeeded but the matching tasks/get always answered "not found".
        if ttl_seconds <= 0:
            raise ValueError(f"ttl_seconds must be positive, got {ttl_seconds!r}")
        if max_tasks <= 0:
            raise ValueError(f"max_tasks must be positive, got {max_tasks!r}")
        if active_ttl_seconds <= 0:
            raise ValueError(f"active_ttl_seconds must be positive, got {active_ttl_seconds!r}")
        self._ttl = ttl_seconds
        self._max = max_tasks
        # Backstop for tasks that never reach a terminal state. Active tasks are
        # deliberately immune to the normal TTL and to capacity eviction, so
        # without an upper bound a single stuck task leaks forever and, worse,
        # blocks eviction for everything behind it. Set generously: this is a
        # leak guard, not a task deadline.
        self._active_ttl = active_ttl_seconds
        self._clock = clock
        # Called with each reclaimed task id. Lets an owner (A2AProtocol) drop
        # its own per-task bookkeeping in lockstep, so side tables cannot become
        # the new unbounded leak once the store itself is bounded.
        self._on_drop = on_drop
        # task_id -> monotonic time of the last write; used by the active-task
        # backstop, kept for every entry (terminal or not).
        self._stored_at: dict[str, float] = {}
        # Insertion-ordered so the oldest terminal task is evicted first.
        self._tasks: "OrderedDict[str, A2ATask]" = OrderedDict()
        # task_id -> monotonic deadline; only present for terminal tasks.
        self._expire_at: dict[str, float] = {}

    # --- internal maintenance -------------------------------------------------

    def _purge_expired(self) -> None:
        """Drop terminal tasks past their TTL, plus active tasks past the backstop."""
        now = self._clock()
        expired = [tid for tid, deadline in self._expire_at.items() if now >= deadline]
        # Active tasks carry no TTL deadline by design, so a task that never
        # reaches a terminal state would otherwise live forever. Reclaim those
        # that have sat unwritten for longer than the backstop window.
        expired.extend(
            tid for tid, stored in self._stored_at.items()
            if tid not in self._expire_at and now - stored >= self._active_ttl
        )
        for tid in expired:
            self._drop(tid)

    def _drop(self, tid: str) -> None:
        """Remove a task from all three books, keeping them in lockstep."""
        self._tasks.pop(tid, None)
        self._expire_at.pop(tid, None)
        self._stored_at.pop(tid, None)
        if self._on_drop is not None:
            try:
                self._on_drop(tid)
            except Exception:  # pragma: no cover - owner bookkeeping is best-effort
                pass

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
            self._drop(victim)

    @staticmethod
    def _is_terminal(task: A2ATask) -> bool:
        return task.state in _TERMINAL_STATES

    # --- dict-like protocol ---------------------------------------------------

    @staticmethod
    def storage_key(owner: str, task_id: str) -> str:
        """Expose the authoritative owner-aware key to protocol side tables."""
        return _owned_storage_key(owner, task_id)

    def get_owned(self, owner: str, task_id: str, default: Any = None) -> Any:
        return self.get(_owned_storage_key(owner, task_id), default)

    def set_owned(self, owner: str, task: A2ATask) -> None:
        task.owner = owner or DEFAULT_TASK_OWNER
        self[_owned_storage_key(task.owner, task.id)] = task

    def active_storage_keys(self) -> set[str]:
        """Snapshot store records that currently represent active work.

        Admission control belongs to ``A2AProtocol._has_active_capacity``, which
        unions this snapshot with its live ``_runs`` handles. A store-only count
        would undercount: a worker that swallowed its cancellation still holds
        resources after the active-TTL backstop reclaimed its record. Do not add
        a capacity predicate here — two competing bounds is how the weaker one
        gets used by mistake.
        """
        self._purge_expired()
        return {
            task_id for task_id in self._tasks
            if task_id not in self._expire_at
        }

    @property
    def max_tasks(self) -> int:
        return self._max

    def __setitem__(self, key: str, task: A2ATask) -> None:
        # Purge before inserting. Expiry used to run only on the read paths, so a
        # workload that only ever writes (anonymous tasks that never get fetched)
        # let the table grow past max_tasks even when every entry was long past
        # its TTL — nothing reclaimed them until some later read happened to run.
        # Purging here also means capacity eviction sees an already-clean table,
        # so it evicts live-but-old entries only when there is genuinely no
        # expired one to reclaim first.
        self._purge_expired()
        self._tasks[key] = task
        self._tasks.move_to_end(key)  # freshest at the end for LRU eviction
        self._stored_at[key] = self._clock()
        if self._is_terminal(task):
            # (Re)arm TTL from now — covers both fresh terminal inserts and a
            # WORKING task that has just transitioned to a terminal state.
            self._expire_at[key] = self._clock() + self._ttl
        else:
            # Still active: immune to the terminal TTL until it becomes terminal.
            # The active backstop in _purge_expired is what bounds it.
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
