"""Scheduler — cron, interval, condition, event, and delay triggers.

Supports: pause/resume/cancel, background tasks with progress tracking,
result delivery, interrupt recovery, and idempotency control.
"""

from __future__ import annotations

import asyncio
import json
import os
import time
import uuid
from dataclasses import dataclass, field
from datetime import datetime
from enum import Enum
from pathlib import Path
from typing import Any, Awaitable, Callable

from loguru import logger

try:
    import fcntl
    _HAS_FCNTL = True
except ImportError:
    fcntl = None  # type: ignore[assignment]
    _HAS_FCNTL = False


class TriggerKind(str, Enum):
    CRON = "cron"
    INTERVAL = "interval"
    ONCE = "once"
    EVENT = "event"


class JobStatus(str, Enum):
    ACTIVE = "active"
    PAUSED = "paused"
    CANCELLED = "cancelled"
    COMPLETED = "completed"


@dataclass
class ScheduledJob:
    id: str = field(default_factory=lambda: uuid.uuid4().hex[:10])
    name: str = ""
    trigger: TriggerKind = TriggerKind.INTERVAL
    cron_expr: str = ""
    interval_ms: int = 0
    at_ms: int = 0
    timezone: str = ""
    event_name: str = ""
    payload: dict[str, Any] = field(default_factory=dict)
    status: JobStatus = JobStatus.ACTIVE
    enabled: bool = True
    delete_after_run: bool = False
    next_run_ms: int | None = None
    last_run_ms: int | None = None
    last_status: str = ""
    last_error: str = ""
    run_count: int = 0
    created_at_ms: int = field(default_factory=lambda: int(time.time() * 1000))

    def to_dict(self) -> dict[str, Any]:
        return {
            "id": self.id, "name": self.name, "trigger": self.trigger.value,
            "cron_expr": self.cron_expr, "interval_ms": self.interval_ms,
            "at_ms": self.at_ms, "timezone": self.timezone,
            "event_name": self.event_name, "payload": self.payload,
            "status": self.status.value, "enabled": self.enabled,
            "delete_after_run": self.delete_after_run,
            "next_run_ms": self.next_run_ms, "last_run_ms": self.last_run_ms,
            "last_status": self.last_status, "last_error": self.last_error,
            "run_count": self.run_count, "created_at_ms": self.created_at_ms,
        }

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> ScheduledJob:
        return cls(
            id=data.get("id", uuid.uuid4().hex[:10]),
            name=data.get("name", ""),
            trigger=TriggerKind(data.get("trigger", "interval")),
            cron_expr=data.get("cron_expr", ""),
            interval_ms=data.get("interval_ms", 0),
            at_ms=data.get("at_ms", 0),
            timezone=data.get("timezone", ""),
            event_name=data.get("event_name", ""),
            payload=data.get("payload", {}),
            status=JobStatus(data.get("status", "active")),
            enabled=data.get("enabled", True),
            delete_after_run=data.get("delete_after_run", False),
            next_run_ms=data.get("next_run_ms"),
            last_run_ms=data.get("last_run_ms"),
            last_status=data.get("last_status", ""),
            last_error=data.get("last_error", ""),
            run_count=data.get("run_count", 0),
            created_at_ms=data.get("created_at_ms", 0),
        )


def _now_ms() -> int:
    return int(time.time() * 1000)


def _compute_next_run(job: ScheduledJob, now_ms: int) -> int | None:
    if job.trigger == TriggerKind.ONCE:
        return job.at_ms if job.at_ms > now_ms else None
    if job.trigger == TriggerKind.INTERVAL:
        return now_ms + job.interval_ms if job.interval_ms > 0 else None
    if job.trigger == TriggerKind.CRON and job.cron_expr:
        try:
            from croniter import croniter
            if job.timezone:
                try:
                    from zoneinfo import ZoneInfo
                    tz = ZoneInfo(job.timezone)
                    base = datetime.fromtimestamp(now_ms / 1000, tz=tz)
                except Exception as tz_e:
                    logger.debug("Invalid timezone '{}', falling back to local: {}", job.timezone, tz_e)
                    base = datetime.fromtimestamp(now_ms / 1000)
            else:
                base = datetime.fromtimestamp(now_ms / 1000)
            cron = croniter(job.cron_expr, base)
            return int(cron.get_next(datetime).timestamp() * 1000)
        except Exception as e:
            # A bad cron expression means this job will never fire again —
            # that must be visible, not a debug whisper.
            logger.warning("Job {} ('{}'): failed to compute next cron run, job will not fire: {}", job.id, job.name, e)
            return None
    return None


class Scheduler:
    """Central scheduler for all trigger types with persistence and recovery."""

    def __init__(
        self,
        store_path: Path,
        on_job: Callable[[ScheduledJob], Awaitable[str | None]] | None = None,
        max_concurrent: int = 10,
    ):
        self._store_path = store_path
        self._on_job = on_job
        self._max_concurrent = max_concurrent
        self._jobs: dict[str, ScheduledJob] = {}
        self._running = False
        self._timer_task: asyncio.Task | None = None
        self._event_handlers: dict[str, list[str]] = {}
        self._background_tasks: dict[str, asyncio.Task] = {}
        self._lock_dir = store_path.parent / "scheduler_locks"
        self._lock_dir.mkdir(parents=True, exist_ok=True)
        self._concurrency_sem: asyncio.Semaphore | None = None
        self._tick_tasks: set[asyncio.Task] = set()
        self._inflight_jobs: set[str] = set()
        self._load()

    def _load(self) -> None:
        if not self._store_path.exists():
            return
        try:
            data = json.loads(self._store_path.read_text(encoding="utf-8"))
            for item in data.get("jobs", []):
                job = ScheduledJob.from_dict(item)
                self._jobs[job.id] = job
                if job.trigger == TriggerKind.EVENT and job.event_name:
                    self._event_handlers.setdefault(job.event_name, []).append(job.id)
        except Exception as e:
            logger.warning("Failed to load scheduler state: {}", e)

    def save_state(self) -> None:
        """Persist current job state to disk."""
        self._save()

    def _save(self) -> None:
        self._write_payload(self._build_payload())

    def _build_payload(self) -> str:
        data = {"jobs": [j.to_dict() for j in self._jobs.values()]}
        return json.dumps(data, ensure_ascii=False, indent=2)

    async def _save_async(self) -> None:
        # Build the payload on the event loop (where _jobs is mutated), then
        # do the fsync'd write in a thread so frequent job runs don't stall
        # the loop on disk I/O.
        payload = self._build_payload()
        await asyncio.to_thread(self._write_payload, payload)

    def _write_payload(self, payload: str) -> None:
        self._store_path.parent.mkdir(parents=True, exist_ok=True)
        # Atomic write: tempfile in the same directory + os.replace.
        # A crash mid-write must never leave the JSON truncated, otherwise
        # _load on next startup loses every scheduled job.
        import tempfile as _tempfile
        fd, tmp = _tempfile.mkstemp(
            dir=str(self._store_path.parent),
            prefix=".scheduler_",
            suffix=".tmp",
        )
        try:
            with os.fdopen(fd, "w", encoding="utf-8") as f:
                f.write(payload)
                f.flush()
                os.fsync(f.fileno())
            os.replace(tmp, str(self._store_path))
        except BaseException:
            try:
                os.unlink(tmp)
            except OSError:
                pass
            raise

    async def start(self) -> None:
        self._running = True
        self._concurrency_sem = asyncio.Semaphore(self._max_concurrent)
        now = _now_ms()
        for job in self._jobs.values():
            if not (job.enabled and job.status == JobStatus.ACTIVE):
                continue
            if job.trigger == TriggerKind.ONCE and job.at_ms and job.at_ms <= now:
                # Due during downtime — fire on the first tick instead of
                # silently never firing while staying ACTIVE forever.
                job.next_run_ms = now
                logger.warning("Job {} ('{}') was due during downtime; firing now", job.id, job.name)
                continue
            if (
                job.trigger == TriggerKind.CRON
                and job.last_run_ms
                and job.next_run_ms
                and job.next_run_ms <= now
            ):
                logger.warning(
                    "Job {} ('{}'): cron occurrence(s) between {} and now were missed during downtime and are skipped",
                    job.id, job.name, datetime.fromtimestamp(job.next_run_ms / 1000).isoformat(),
                )
            job.next_run_ms = _compute_next_run(job, now)
        await self._save_async()
        self._timer_task = asyncio.create_task(self._tick_loop())
        logger.info("Scheduler started with {} jobs", len(self._jobs))

    async def stop(self) -> None:
        self._running = False
        if self._timer_task:
            self._timer_task.cancel()
            try:
                await self._timer_task
            except asyncio.CancelledError:
                pass
        for task in list(self._tick_tasks):
            task.cancel()
        if self._tick_tasks:
            await asyncio.gather(*self._tick_tasks, return_exceptions=True)
            self._tick_tasks.clear()
        self._inflight_jobs.clear()
        for task in self._background_tasks.values():
            task.cancel()
        self._save()

    def add_job(self, job: ScheduledJob) -> ScheduledJob:
        job.next_run_ms = _compute_next_run(job, _now_ms())
        self._jobs[job.id] = job
        if job.trigger == TriggerKind.EVENT and job.event_name:
            self._event_handlers.setdefault(job.event_name, []).append(job.id)
        self._save()
        return job

    def remove_job(self, job_id: str) -> bool:
        job = self._jobs.pop(job_id, None)
        if job:
            self._save()
            return True
        return False

    def list_jobs(self) -> list[ScheduledJob]:
        return list(self._jobs.values())

    def get_job(self, job_id: str) -> ScheduledJob | None:
        return self._jobs.get(job_id)

    def get_run_history(self, job_id: str, limit: int = 10) -> list[dict[str, Any]]:
        """Return recent run info for a job (derived from current state).

        The scheduler does not persist a full run log today; return a
        summary based on the last execution so callers have something
        to display. A future iteration can persist per-run records.
        """
        job = self._jobs.get(job_id)
        if not job or not job.last_run_ms:
            return []
        return [{
            "ts": job.last_run_ms,
            "status": job.last_status or "unknown",
            "error": job.last_error,
            "run_count": job.run_count,
        }]

    async def record_run_outcome(self, job_id: str, status: str, error: str = "") -> None:
        """Write back the real end-to-end outcome of a dispatched job run.

        _run_job only knows the event was *queued* onto the bus; the agent turn
        (and any user-facing delivery) completes asynchronously afterwards, in
        the loop. This lets that downstream completion update last_status to a
        truthful terminal value ("completed" / "error") instead of leaving it
        stuck at "queued". No-op if the job is gone (e.g. a run-once job already
        pruned) so a late writeback can never resurrect scheduler state."""
        job = self._jobs.get(job_id)
        if job is None:
            return
        job.last_status = status
        job.last_error = error
        await self._save_async()

    async def trigger_job(self, job_id: str) -> bool:
        job = self._jobs.get(job_id)
        if not job or not job.enabled or job.status in (JobStatus.CANCELLED, JobStatus.COMPLETED):
            return False
        await self._execute_job(job)
        return True

    async def _tick_loop(self) -> None:
        while self._running:
            try:
                now = _now_ms()
                for job in list(self._jobs.values()):
                    if not job.enabled or job.status != JobStatus.ACTIVE:
                        continue
                    if not job.next_run_ms or now < job.next_run_ms:
                        continue
                    if job.id in self._inflight_jobs:
                        # Previous run still executing — skip this tick rather
                        # than queue a duplicate, otherwise a slow job whose
                        # interval is shorter than its runtime would pile up.
                        continue
                    self._inflight_jobs.add(job.id)
                    task = asyncio.create_task(self._tick_dispatch(job))
                    self._tick_tasks.add(task)
                    task.add_done_callback(self._tick_tasks.discard)
                await asyncio.sleep(1)
            except asyncio.CancelledError:
                break
            except Exception as e:
                logger.error("Scheduler tick error: {}", e)
                await asyncio.sleep(5)

    async def _tick_dispatch(self, job: ScheduledJob) -> None:
        sem = self._concurrency_sem
        try:
            if sem is not None:
                async with sem:
                    await self._execute_job(job)
            else:
                await self._execute_job(job)
        except Exception as e:
            logger.error("Tick dispatch for job {} crashed: {}", job.id, e)
        finally:
            self._inflight_jobs.discard(job.id)

    async def _execute_job(self, job: ScheduledJob) -> None:
        lock_fd = self._try_acquire_lock(job.id)
        if lock_fd is None:
            logger.debug("Job {} locked by another instance, skipping", job.id)
            return
        try:
            await self._run_job(job)
        finally:
            self._release_lock(lock_fd)

    async def _run_job(self, job: ScheduledJob) -> None:
        job.last_run_ms = _now_ms()
        job.run_count += 1
        try:
            if self._on_job:
                # The handler returns what actually happened. For bus-dispatched
                # jobs this is "queued" — the event was accepted into the bus but
                # the agent runs (and any delivery) asynchronously afterwards.
                # Recording "success" here would be a false signal: it would only
                # ever mean "enqueued", masking downstream execution/delivery
                # failures. Honour the handler's own status instead.
                status = await self._on_job(job)
                job.last_status = status or "queued"
                job.last_error = ""
            else:
                job.last_status = "skipped"
        except Exception as e:
            job.last_status = "error"
            job.last_error = str(e)
            logger.error("Job {} failed: {}", job.id, e)

        if job.delete_after_run or job.trigger == TriggerKind.ONCE:
            job.status = JobStatus.COMPLETED
        else:
            job.next_run_ms = _compute_next_run(job, _now_ms())
            if job.next_run_ms is None:
                job.status = JobStatus.COMPLETED
                logger.warning("Job {} ('{}') has no computable next run; marking completed", job.id, job.name)

        await self._save_async()

    def _try_acquire_lock(self, job_id: str) -> Any:
        if not _HAS_FCNTL:
            return True
        lock_path = self._lock_dir / f"{job_id}.lock"
        fd = None
        try:
            fd = open(lock_path, "w")
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            return fd
        except (IOError, OSError):
            if fd is not None:
                fd.close()
            return None

    def _release_lock(self, lock_fd: Any) -> None:
        if lock_fd is True:
            return
        try:
            fcntl.flock(lock_fd, fcntl.LOCK_UN)
            lock_fd.close()
        except (IOError, OSError):
            pass
