"""Small in-process job registry for long-running Dashboard operations.

Jobs are deliberately ephemeral: the durable source of truth remains the
subsystem being rebuilt (for example the knowledge index). A gateway restart
turns an in-flight job into "unknown" while the index's own stale/status fields
still describe whether another rebuild is required.
"""

from __future__ import annotations

import asyncio
import inspect
import uuid
from collections.abc import Awaitable, Callable
from datetime import datetime, timezone
from typing import Any


def _iso_now() -> str:
    return datetime.now(timezone.utc).isoformat()


class AsyncJobRegistry:
    """Own bounded task state and publish lifecycle changes best-effort."""

    def __init__(
        self,
        *,
        event_sink: Callable[[str, dict[str, Any]], Awaitable[Any]] | None = None,
        event_type: str = "job_updated",
        max_jobs: int = 100,
    ) -> None:
        self._event_sink = event_sink
        self._event_type = event_type
        self._max_jobs = max(10, int(max_jobs))
        self._jobs: dict[str, dict[str, Any]] = {}
        self._tasks: dict[str, asyncio.Task[Any]] = {}

    def start(
        self,
        action: str,
        work: Callable[[], Awaitable[Any]],
        *,
        metadata: dict[str, Any] | None = None,
    ) -> dict[str, Any]:
        job_id = uuid.uuid4().hex[:12]
        now = _iso_now()
        self._jobs[job_id] = {
            "id": job_id,
            "action": action,
            "status": "queued",
            "progress": 0,
            "message": "",
            "result": None,
            "error": "",
            "metadata": dict(metadata or {}),
            "created_at": now,
            "started_at": None,
            "completed_at": None,
        }
        task = asyncio.create_task(self._run(job_id, work), name=f"dashboard-job:{action}:{job_id}")
        self._tasks[job_id] = task
        task.add_done_callback(lambda _task, jid=job_id: self._task_done(jid, _task))
        self._trim()
        self._schedule_emit(job_id)
        return self.get(job_id) or {}

    async def _run(self, job_id: str, work: Callable[[], Awaitable[Any]]) -> None:
        job = self._jobs[job_id]
        job.update(status="running", progress=5, started_at=_iso_now())
        await self._emit(job_id)
        try:
            result = work()
            if inspect.isawaitable(result):
                result = await result
        except asyncio.CancelledError:
            job.update(
                status="cancelled",
                progress=100,
                message="cancelled",
                completed_at=_iso_now(),
            )
            await self._emit(job_id)
            return
        except Exception as exc:  # noqa: BLE001 - background job owns its failure
            job.update(
                status="failed",
                progress=100,
                error=str(exc),
                completed_at=_iso_now(),
            )
            await self._emit(job_id)
            return
        job.update(
            status="completed",
            progress=100,
            result=result,
            completed_at=_iso_now(),
        )
        await self._emit(job_id)

    def get(self, job_id: str) -> dict[str, Any] | None:
        job = self._jobs.get(job_id)
        return dict(job) if job is not None else None

    def list(self, limit: int = 20) -> list[dict[str, Any]]:
        jobs = list(self._jobs.values())[-max(1, min(int(limit), self._max_jobs)) :]
        return [dict(job) for job in reversed(jobs)]

    async def cancel(self, job_id: str) -> bool:
        task = self._tasks.get(job_id)
        job = self._jobs.get(job_id)
        if task is None or job is None or job["status"] not in {"queued", "running"}:
            return False
        task.cancel()
        try:
            await task
        except asyncio.CancelledError:
            # The state normalization below owns the observable terminal result.
            pass
        # A task can be cancelled before its coroutine gets its first timeslice,
        # so _run's CancelledError handler is not guaranteed to execute. Ensure
        # every accepted cancellation has a terminal, observable state.
        if job["status"] in {"queued", "running"}:
            self._mark_cancelled(job_id)
            await self._emit(job_id)
        # Work may deliberately suppress cancellation after crossing an
        # irreversible commit boundary. In that race the truthful terminal
        # state is completed/failed and the cancel endpoint must not claim
        # success merely because Task.cancel() was called.
        return job["status"] == "cancelled"

    async def close(self) -> None:
        tasks = list(self._tasks.values())
        for task in tasks:
            task.cancel()
        if tasks:
            await asyncio.gather(*tasks, return_exceptions=True)
        self._tasks.clear()

    def _trim(self) -> None:
        while len(self._jobs) > self._max_jobs:
            oldest_finished = next(
                (job_id for job_id in self._jobs if job_id not in self._tasks),
                None,
            )
            if oldest_finished is None:
                break
            self._jobs.pop(oldest_finished, None)

    def _mark_cancelled(self, job_id: str) -> None:
        job = self._jobs.get(job_id)
        if job is None or job["status"] not in {"queued", "running"}:
            return
        job.update(
            status="cancelled",
            progress=100,
            message="cancelled",
            completed_at=_iso_now(),
        )

    def _task_done(self, job_id: str, task: asyncio.Task[Any]) -> None:
        self._tasks.pop(job_id, None)
        if task.cancelled():
            # Defensive fallback for cancellation performed by close() or by a
            # caller that holds the task. Scheduling the event is best-effort.
            self._mark_cancelled(job_id)
            self._schedule_emit(job_id)
        self._trim()

    def _schedule_emit(self, job_id: str) -> None:
        if self._event_sink is None:
            return
        try:
            asyncio.get_running_loop().create_task(self._emit(job_id))
        except RuntimeError:
            return

    async def _emit(self, job_id: str) -> None:
        if self._event_sink is None:
            return
        job = self.get(job_id)
        if job is None:
            return
        try:
            await self._event_sink(self._event_type, job)
        except Exception:
            # Observability must never change the outcome of the owned operation.
            pass
