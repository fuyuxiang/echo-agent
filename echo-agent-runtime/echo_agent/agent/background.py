"""Tiered, bounded background task scheduler.

DISCARDABLE: reflection / review / prefetch — dropped under saturation,
             failures only logged. Losing one is harmless (recomputed next turn).
DURABLE:     memory writes / consolidation commits — queued under saturation,
             retried on failure. Never silently dropped.

API note — bare coroutine vs factory:
    A coroutine object can only be awaited once, so DURABLE retries cannot reuse
    a pre-built coroutine. ``spawn`` therefore accepts *either*:
      - a bare coroutine (``async def f(): ...`` called as ``f()``), or
      - a zero-arg factory (``Callable[[], Awaitable]``, e.g. ``lambda: f()``).
    The two are auto-detected. DISCARDABLE never retries, so a bare coroutine is
    fine. DURABLE retries only re-run when given a factory; a bare coroutine is
    awaited once and (if it fails) not retried, because there is nothing to
    re-await. Callers that need retry semantics pass a factory.
"""
from __future__ import annotations

import asyncio
import enum
import inspect
from typing import Any, Awaitable, Callable, Union

from loguru import logger

# Either a ready-made awaitable or a zero-arg factory that produces a fresh one.
Spawnable = Union[Awaitable[Any], Callable[[], Awaitable[Any]]]


class Tier(enum.Enum):
    DISCARDABLE = "discardable"
    DURABLE = "durable"


class BackgroundScheduler:
    """Bounded, tiered scheduler for fire-and-forget background work."""

    def __init__(
        self,
        max_concurrency: int,
        *,
        durable_retries: int = 2,
        durable_queue_warn: int | None = None,
    ) -> None:
        self._max_concurrency = max(1, max_concurrency)
        self._sem = asyncio.Semaphore(self._max_concurrency)
        self._durable_retries = max(0, durable_retries)
        # High-water mark for queued (not-yet-running) DURABLE work. DURABLE is
        # never dropped, so a slow consumer can let the queue grow; this is the
        # threshold past which we log a warning so the backlog is observable
        # rather than silently accumulating. Defaults to 4x concurrency.
        self._durable_queue_warn = (
            durable_queue_warn if durable_queue_warn is not None
            else self._max_concurrency * 4
        )
        self._tasks: set[asyncio.Task[Any]] = set()
        self._discardable_tasks: set[asyncio.Task[Any]] = set()
        self._dropped = 0
        self._queued = 0
        # Saturation counter for the DISCARDABLE drop decision. It counts work
        # that is *occupying or about to occupy* a concurrency slot:
        #   - every DISCARDABLE task from the moment it is spawned, and
        #   - every DURABLE task only once it is actually RUNNING (holds the sem).
        # A DURABLE task merely *queued* on the semaphore is deliberately NOT
        # counted: otherwise a burst of queued durable work (e.g. memory writes
        # under load) would pin this at the cap and cause every retrieval
        # prefetch to be dropped — silently defeating the prefetch latency win
        # exactly when the system is busy. Maintained synchronously for
        # DISCARDABLE (incremented in ``spawn``) so a synchronous burst is
        # bounded before any task reaches its first await.
        self._slot_load = 0

    def spawn(
        self,
        coro: Spawnable,
        *,
        tier: Tier = Tier.DISCARDABLE,
    ) -> None:
        """Schedule ``coro`` under the given tier.

        ``coro`` may be a bare coroutine/awaitable or a zero-arg factory
        returning a fresh awaitable. DURABLE retries require a factory.
        """
        if tier is Tier.DISCARDABLE and self._slot_load >= self._max_concurrency:
            # No free slot — drop and close any live coroutine to avoid a
            # "coroutine was never awaited" warning. Using the synchronous
            # slot-load count (not ``sem.locked()``) means a burst of spawns
            # with no await in between is still bounded: once the cap is hit,
            # further DISCARDABLE work is dropped instead of queued unbounded.
            self._dropped += 1
            logger.debug("Dropped discardable background task (saturated)")
            self._discard(coro)
            return

        if tier is Tier.DISCARDABLE:
            self._slot_load += 1
        task = asyncio.create_task(self._run(coro, tier))
        self._tasks.add(task)
        task.add_done_callback(self._tasks.discard)
        if tier is Tier.DISCARDABLE:
            self._discardable_tasks.add(task)
            task.add_done_callback(self._discardable_tasks.discard)

    @staticmethod
    def _discard(coro: Spawnable) -> None:
        """Close a coroutine we never awaited; no-op for factories."""
        if inspect.iscoroutine(coro):
            coro.close()

    async def _run(self, coro: Spawnable, tier: Tier) -> None:
        queued = False
        invoked = False
        durable_running = False
        try:
            if tier is Tier.DURABLE and self._sem.locked():
                self._queued += 1
                queued = True
                # DURABLE is never dropped, so the queue can grow under a slow
                # consumer. Surface a backlog instead of letting it accumulate
                # invisibly — the only OOM vector left once durable no longer
                # pins the discardable saturation counter.
                if self._queued >= self._durable_queue_warn:
                    logger.warning(
                        "DURABLE background queue backlog: {} waiting (warn at {})",
                        self._queued, self._durable_queue_warn,
                    )
            async with self._sem:
                if queued:
                    self._queued -= 1
                    queued = False
                # Past this point the user coro is reached without any further
                # suspension, so once ``invoked`` is set, ``coro`` is guaranteed
                # to have been entered (awaited) and needs no defensive close.
                invoked = True
                if tier is Tier.DURABLE:
                    # Count durable against the discardable saturation cap only
                    # while it actually holds a concurrency slot (running), not
                    # while it was merely queued above.
                    self._slot_load += 1
                    durable_running = True
                    await self._run_durable(coro)
                else:
                    await self._run_discardable(coro)
        finally:
            if not invoked:
                # Cancelled (e.g. by ``aclose``) or failed before we ever
                # awaited the user coro — typically while still blocked on the
                # semaphore. Close the bare coroutine so it does not trigger a
                # "coroutine was never awaited" warning. No-op for factories.
                self._discard(coro)
            if queued:
                self._queued -= 1
            # Release the saturation slot: DISCARDABLE held it from ``spawn``;
            # DURABLE held it only while running. Balanced 1:1 with each
            # increment so the counter never drifts negative or leaks a slot.
            if tier is Tier.DISCARDABLE or durable_running:
                self._slot_load -= 1

    async def _run_discardable(self, coro: Spawnable) -> None:
        try:
            await self._invoke(coro)
        except asyncio.CancelledError:
            raise
        except Exception as e:  # noqa: BLE001
            logger.warning("Discardable background task failed: {}", e)

    async def _run_durable(self, coro: Spawnable) -> None:
        is_factory = not inspect.iscoroutine(coro) and callable(coro)
        last_exc: Exception | None = None
        for attempt in range(self._durable_retries + 1):
            try:
                await self._invoke(coro)
                return
            except asyncio.CancelledError:
                raise
            except Exception as e:  # noqa: BLE001
                last_exc = e
                logger.warning(
                    "Durable task failed (attempt {}): {}", attempt + 1, e
                )
                # A bare coroutine is already consumed and cannot be re-awaited,
                # so retry only when we hold a factory that yields fresh coros.
                if not is_factory or attempt >= self._durable_retries:
                    break
                await asyncio.sleep(0.1 * (2 ** attempt))
        if last_exc is not None:
            logger.error("Durable task gave up after retries: {}", last_exc)

    @staticmethod
    async def _invoke(coro: Spawnable) -> None:
        """Await a bare coroutine, or call a factory then await its result."""
        if inspect.iscoroutine(coro) or inspect.isawaitable(coro):
            await coro
        else:
            await coro()  # type: ignore[misc]

    def stats(self) -> dict[str, int]:
        running = sum(1 for t in self._tasks if not t.done())
        return {"running": running, "queued": self._queued, "dropped": self._dropped}

    async def aclose(self, timeout: float = 10.0) -> None:
        """Cancel in-flight DISCARDABLE tasks; flush/await DURABLE tasks."""
        # Yield once so freshly created tasks reach their first suspension
        # point inside ``_run``. Without this, a task created during a
        # synchronous burst may still be un-started; cancelling it closes
        # ``_run`` before its body runs, so the ``finally`` cleanup never fires
        # and the bare user coroutine leaks a "never awaited" warning.
        await asyncio.sleep(0)
        for task in list(self._discardable_tasks):
            task.cancel()
        if not self._tasks:
            return
        try:
            await asyncio.wait_for(
                asyncio.gather(*list(self._tasks), return_exceptions=True),
                timeout=timeout,
            )
        except asyncio.TimeoutError:
            logger.warning("BackgroundScheduler.aclose timed out after {}s", timeout)
