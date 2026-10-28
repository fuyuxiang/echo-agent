"""Bounded, concurrency-safe idempotency records for inbound HTTP/WS events."""

from __future__ import annotations

import asyncio
import hashlib
import json
import time
from collections import OrderedDict
from dataclasses import dataclass, field
from typing import Any, Callable


_MAX_KEY_CHARS = 200
IDEMPOTENCY_NAMESPACE_METADATA = "_idempotency_namespace"
IDEMPOTENCY_FINGERPRINT_METADATA = "_idempotency_fingerprint"


def normalize_idempotency_key(value: Any) -> str:
    """Validate a caller-provided idempotency key, returning ``""`` if absent."""
    if value is None or value == "":
        return ""
    if not isinstance(value, str):
        raise ValueError("idempotency key must be a string")
    key = value.strip()
    if not key:
        raise ValueError("idempotency key must not be blank")
    if len(key) > _MAX_KEY_CHARS:
        raise ValueError(f"idempotency key exceeds {_MAX_KEY_CHARS} characters")
    if any(ord(ch) < 32 or ord(ch) == 127 for ch in key):
        raise ValueError("idempotency key contains control characters")
    return key


def deterministic_event_id(namespace: str, scope: str, key: str) -> str:
    """Create the durable event ID claimed by the turn ledger for this key."""
    material = f"{namespace}\0{scope}\0{key}".encode("utf-8")
    return hashlib.sha256(material).hexdigest()[:32]


def canonical_operation_fingerprint(operation: Any) -> str:
    """Hash the parsed operation, independent of JSON encoding and retry controls.

    Callers must pass only fields that affect the operation itself. Transport
    placement of the idempotency key and response controls such as ``wait`` or
    ``timeout`` deliberately do not belong in this object.
    """
    canonical = json.dumps(
        operation,
        sort_keys=True,
        separators=(",", ":"),
        ensure_ascii=False,
        default=str,
    )
    return hashlib.sha256(canonical.encode("utf-8")).hexdigest()


def idempotency_ledger_metadata(
    metadata: Any,
) -> dict[str, str]:
    """Select validated idempotency binding fields for durable turn storage."""
    if not isinstance(metadata, dict):
        return {}
    namespace = metadata.get(IDEMPOTENCY_NAMESPACE_METADATA)
    fingerprint = metadata.get(IDEMPOTENCY_FINGERPRINT_METADATA)
    if not isinstance(namespace, str) or not namespace:
        return {}
    if not isinstance(fingerprint, str) or not fingerprint:
        return {}
    return {
        IDEMPOTENCY_NAMESPACE_METADATA: namespace,
        IDEMPOTENCY_FINGERPRINT_METADATA: fingerprint,
    }


def durable_fingerprint_conflicts(
    row: dict[str, Any], *, namespace: str, fingerprint: str,
) -> bool:
    """Detect changed-operation reuse against a durable turn ledger row.

    Old rows created before fingerprint persistence remain replay-only for
    compatibility: the deterministic event ID still prevents duplicate work,
    but there is no historical fact with which to prove a conflict.
    """
    metadata = row.get("metadata")
    if not isinstance(metadata, dict):
        return False
    stored_fingerprint = metadata.get(IDEMPOTENCY_FINGERPRINT_METADATA)
    if not isinstance(stored_fingerprint, str) or not stored_fingerprint:
        return False
    return (
        metadata.get(IDEMPOTENCY_NAMESPACE_METADATA) != namespace
        or stored_fingerprint != fingerprint
    )


@dataclass(frozen=True)
class CachedResponse:
    status: int
    payload: dict[str, Any]


@dataclass
class IdempotencyEntry:
    cache_key: str
    event_id: str
    fingerprint: str
    created_at: float
    context: dict[str, Any] = field(default_factory=dict)
    admission: CachedResponse | None = None
    response: CachedResponse | None = None
    admission_future: asyncio.Future[CachedResponse] | None = field(
        default=None, repr=False,
    )
    future: asyncio.Future[CachedResponse] | None = field(default=None, repr=False)


@dataclass(frozen=True)
class IdempotencyClaim:
    outcome: str  # "new", "duplicate", "conflict", or "full"
    entry: IdempotencyEntry | None = None


class BoundedIdempotencyStore:
    """TTL/LRU store that atomically arbitrates concurrent duplicate requests.

    Completed entries are evicted oldest-first at capacity. In-flight entries
    are retained so a retry cannot execute twice merely because the cache is
    busy; if every slot is in flight, a new key is rejected until one settles.
    ``ttl_seconds`` is also the stale in-flight backstop: after that window the
    outcome is explicitly reported unknown and the slot is reclaimed, preventing
    crashed handlers from pinning the bounded store forever.
    """

    def __init__(
        self,
        *,
        max_entries: int = 2048,
        ttl_seconds: float = 3600.0,
        clock: Callable[[], float] = time.monotonic,
    ) -> None:
        if max_entries <= 0:
            raise ValueError("max_entries must be positive")
        if ttl_seconds <= 0:
            raise ValueError("ttl_seconds must be positive")
        self._max_entries = max_entries
        self._ttl_seconds = ttl_seconds
        self._clock = clock
        self._entries: OrderedDict[str, IdempotencyEntry] = OrderedDict()
        self._event_keys: dict[str, str] = {}
        self._lock = asyncio.Lock()

    @staticmethod
    def _cache_key(namespace: str, scope: str, key: str) -> str:
        return hashlib.sha256(
            f"{namespace}\0{scope}\0{key}".encode("utf-8")
        ).hexdigest()

    def _drop(self, cache_key: str, *, stale: bool = False) -> None:
        entry = self._entries.pop(cache_key, None)
        if entry is None:
            return
        self._event_keys.pop(entry.event_id, None)
        if stale:
            response = CachedResponse(
                status=409,
                payload={"error": "idempotency record expired; outcome unknown"},
            )
            if entry.admission_future is not None and not entry.admission_future.done():
                entry.admission_future.set_result(response)
            if entry.future is not None and not entry.future.done():
                entry.future.set_result(response)

    def _purge_expired(self) -> None:
        now = self._clock()
        expired = [
            cache_key
            for cache_key, entry in self._entries.items()
            if now - entry.created_at >= self._ttl_seconds
        ]
        for cache_key in expired:
            self._drop(cache_key, stale=True)

    def _make_room(self) -> bool:
        if len(self._entries) < self._max_entries:
            return True
        victim = next(
            (
                cache_key
                for cache_key, entry in self._entries.items()
                if entry.response is not None
            ),
            None,
        )
        if victim is None:
            return False
        self._drop(victim)
        return True

    async def claim(
        self,
        *,
        namespace: str,
        scope: str,
        key: str,
        fingerprint: str,
        event_id: str,
        context: dict[str, Any] | None = None,
    ) -> IdempotencyClaim:
        cache_key = self._cache_key(namespace, scope, key)
        async with self._lock:
            self._purge_expired()
            existing = self._entries.get(cache_key)
            if existing is not None:
                self._entries.move_to_end(cache_key)
                if existing.fingerprint != fingerprint:
                    return IdempotencyClaim("conflict", existing)
                return IdempotencyClaim("duplicate", existing)
            if not self._make_room():
                return IdempotencyClaim("full")
            entry = IdempotencyEntry(
                cache_key=cache_key,
                event_id=event_id,
                fingerprint=fingerprint,
                created_at=self._clock(),
                context=dict(context or {}),
                admission_future=asyncio.get_running_loop().create_future(),
                future=asyncio.get_running_loop().create_future(),
            )
            self._entries[cache_key] = entry
            self._event_keys[event_id] = cache_key
            return IdempotencyClaim("new", entry)

    async def mark_admitted(
        self, entry: IdempotencyEntry, *, status: int, payload: dict[str, Any],
    ) -> bool:
        """Record the definite transport admission without ending a wait request."""
        async with self._lock:
            current = self._entries.get(entry.cache_key)
            if current is not entry or current.admission is not None:
                return False
            current.admission = CachedResponse(status=status, payload=dict(payload))
            if current.admission_future is not None and not current.admission_future.done():
                current.admission_future.set_result(current.admission)
            return True

    async def complete(
        self, entry: IdempotencyEntry, *, status: int, payload: dict[str, Any],
    ) -> bool:
        async with self._lock:
            current = self._entries.get(entry.cache_key)
            if current is not entry or current.response is not None:
                return False
            response = CachedResponse(status=status, payload=dict(payload))
            if current.admission is None:
                current.admission = response
                if (
                    current.admission_future is not None
                    and not current.admission_future.done()
                ):
                    current.admission_future.set_result(response)
            current.response = response
            # Completed-response retention starts now. A long-running request
            # must still receive the full replay window after it settles; only
            # the in-flight stale backstop is measured from the original claim.
            current.created_at = self._clock()
            self._entries.move_to_end(entry.cache_key)
            if current.future is not None and not current.future.done():
                current.future.set_result(current.response)
            return True

    async def complete_event(
        self,
        event_id: str,
        *,
        status: int,
        payload: dict[str, Any],
    ) -> bool:
        async with self._lock:
            cache_key = self._event_keys.get(event_id)
            entry = self._entries.get(cache_key or "")
            if entry is None or entry.response is not None:
                return False
            response = CachedResponse(status=status, payload=dict(payload))
            if entry.admission is None:
                entry.admission = response
                if (
                    entry.admission_future is not None
                    and not entry.admission_future.done()
                ):
                    entry.admission_future.set_result(response)
            entry.response = response
            entry.created_at = self._clock()
            self._entries.move_to_end(entry.cache_key)
            if entry.future is not None and not entry.future.done():
                entry.future.set_result(entry.response)
            return True

    async def context_for_event(self, event_id: str) -> dict[str, Any]:
        async with self._lock:
            cache_key = self._event_keys.get(event_id)
            entry = self._entries.get(cache_key or "")
            return dict(entry.context) if entry is not None else {}

    async def abort(
        self,
        entry: IdempotencyEntry,
        *,
        status: int = 503,
        payload: dict[str, Any] | None = None,
    ) -> None:
        async with self._lock:
            current = self._entries.get(entry.cache_key)
            if current is not entry:
                return
            response = CachedResponse(
                status=status,
                payload=dict(payload or {"error": "request was not accepted"}),
            )
            if current.admission_future is not None and not current.admission_future.done():
                current.admission_future.set_result(response)
            if current.future is not None and not current.future.done():
                current.future.set_result(response)
            self._drop(entry.cache_key)

    @staticmethod
    async def wait_admitted(
        entry: IdempotencyEntry, *, timeout: float | None = None,
    ) -> CachedResponse:
        if entry.admission is not None:
            return entry.admission
        if entry.admission_future is None:
            return CachedResponse(status=409, payload={"error": "outcome unavailable"})
        waiter = asyncio.shield(entry.admission_future)
        if timeout is None:
            return await waiter
        return await asyncio.wait_for(waiter, timeout=timeout)

    @staticmethod
    async def wait(
        entry: IdempotencyEntry, *, timeout: float | None = None,
    ) -> CachedResponse:
        if entry.response is not None:
            return entry.response
        if entry.future is None:
            return CachedResponse(status=409, payload={"error": "outcome unavailable"})
        waiter = asyncio.shield(entry.future)
        if timeout is None:
            return await waiter
        return await asyncio.wait_for(waiter, timeout=timeout)

    @property
    def size(self) -> int:
        return len(self._entries)
