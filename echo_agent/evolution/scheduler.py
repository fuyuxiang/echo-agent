"""EvolutionScheduler — chooses *when* to invoke ``EvolutionEngine.run_evolution``.

Three trigger modes (mutually exclusive):

  - manual     : never fires automatically; CLI / agent tools / Gateway only.
  - threshold  : fires whenever the unconsumed-trajectory count crosses N.
  - scheduled  : fires on a cron expression (using croniter).

The scheduler runs a single asyncio polling task; it does not push work into
the global :class:`echo_agent.scheduler.service.Scheduler` because evolution
runs are session-internal and should not survive a restart unfinished.
"""

from __future__ import annotations

import asyncio
from datetime import datetime
from typing import Awaitable, Callable

from loguru import logger

from echo_agent.evolution.types import EvolutionRun


RunEvolutionFn = Callable[..., Awaitable[EvolutionRun]]


class EvolutionScheduler:
    def __init__(
        self,
        *,
        run_fn: RunEvolutionFn,
        unconsumed_count_fn: Callable[[], Awaitable[int]],
        trigger_mode: str = "manual",
        threshold: int = 50,
        cron_expression: str = "0 4 * * *",
        poll_interval_seconds: float = 30.0,
    ):
        self._run_fn = run_fn
        self._count_fn = unconsumed_count_fn
        self._mode = trigger_mode
        self._threshold = max(1, int(threshold))
        self._cron_expression = cron_expression
        self._poll_interval = max(5.0, float(poll_interval_seconds))
        self._task: asyncio.Task | None = None
        self._running = False
        self._next_cron_ts: float | None = None

    @property
    def mode(self) -> str:
        return self._mode

    async def start(self) -> None:
        if self._running or self._mode == "manual":
            return
        self._running = True
        if self._mode == "scheduled":
            self._next_cron_ts = self._compute_next_cron_ts(datetime.now())
        self._task = asyncio.create_task(self._loop(), name="evolution-scheduler")
        logger.info(
            "EvolutionScheduler started (mode={}, threshold={}, cron='{}')",
            self._mode,
            self._threshold,
            self._cron_expression,
        )

    async def stop(self) -> None:
        self._running = False
        if self._task is not None:
            self._task.cancel()
            try:
                await self._task
            except asyncio.CancelledError:
                pass
            except Exception as e:
                logger.debug("EvolutionScheduler stop raised: {}", e)
        self._task = None

    async def _loop(self) -> None:
        try:
            while self._running:
                try:
                    await self._tick()
                except asyncio.CancelledError:
                    raise
                except Exception as e:
                    logger.warning("EvolutionScheduler tick failed: {}", e)
                await asyncio.sleep(self._poll_interval)
        except asyncio.CancelledError:
            return

    async def _tick(self) -> None:
        if self._mode == "threshold":
            await self._tick_threshold()
        elif self._mode == "scheduled":
            await self._tick_cron()

    async def _tick_threshold(self) -> None:
        try:
            count = await self._count_fn()
        except Exception as e:
            logger.debug("threshold count failed: {}", e)
            return
        if count >= self._threshold:
            logger.info("EvolutionScheduler threshold reached ({} >= {})", count, self._threshold)
            await self._safe_run("threshold")

    async def _tick_cron(self) -> None:
        if self._next_cron_ts is None:
            self._next_cron_ts = self._compute_next_cron_ts(datetime.now())
            return
        now_ts = datetime.now().timestamp()
        if now_ts >= self._next_cron_ts:
            await self._safe_run("scheduled")
            self._next_cron_ts = self._compute_next_cron_ts(datetime.now())

    async def _safe_run(self, trigger: str) -> None:
        try:
            await self._run_fn(trigger=trigger)
        except Exception as e:
            logger.warning("Scheduled evolution run failed ({}): {}", trigger, e)

    def _compute_next_cron_ts(self, base: datetime) -> float | None:
        if not self._cron_expression:
            return None
        try:
            from croniter import croniter

            cron = croniter(self._cron_expression, base)
            return cron.get_next(datetime).timestamp()
        except Exception as e:
            logger.warning("Invalid cron expression '{}': {}", self._cron_expression, e)
            return None
