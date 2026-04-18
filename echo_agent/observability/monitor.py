"""Observability — full-chain logging, task views, and health checks.

Covers:
  - TraceLogger: input → context → tools → output → result chain
  - TaskObserver: task state, steps, subtask tree, errors, retries, timing
  - HealthChecker: channel/worker/timer health, dead task cleanup, recovery
"""

from __future__ import annotations

import asyncio
import json
import time
from dataclasses import dataclass, field
from datetime import datetime
from enum import Enum
from pathlib import Path
from typing import Any, Awaitable, Callable

from loguru import logger


# ── Trace Logging ────────────────────────────────────────────────────────────

@dataclass
class TraceSpan:
    span_id: str = ""
    trace_id: str = ""
    parent_id: str = ""
    name: str = ""
    kind: str = ""  # input, context, tool_call, tool_result, llm_call, output
    started_at: float = 0.0
    ended_at: float = 0.0
    metadata: dict[str, Any] = field(default_factory=dict)
    error: str = ""

    @property
    def duration_ms(self) -> int:
        if self.ended_at and self.started_at:
            return int((self.ended_at - self.started_at) * 1000)
        return 0

    def to_dict(self) -> dict[str, Any]:
        return {
            "span_id": self.span_id, "trace_id": self.trace_id,
            "parent_id": self.parent_id, "name": self.name, "kind": self.kind,
            "started_at": self.started_at, "ended_at": self.ended_at,
            "duration_ms": self.duration_ms, "metadata": self.metadata,
            "error": self.error,
        }


class TraceLogger:
    """Records full processing chain for each agent interaction."""

    def __init__(self, logs_dir: Path | None = None):
        self._logs_dir = logs_dir
        if logs_dir:
            logs_dir.mkdir(parents=True, exist_ok=True)
        self._traces: dict[str, list[TraceSpan]] = {}

    def start_span(self, trace_id: str, span_id: str, name: str, kind: str, parent_id: str = "") -> TraceSpan:
        span = TraceSpan(
            span_id=span_id, trace_id=trace_id, parent_id=parent_id,
            name=name, kind=kind, started_at=time.time(),
        )
        self._traces.setdefault(trace_id, []).append(span)
        return span

    def end_span(self, span: TraceSpan, error: str = "", metadata: dict[str, Any] | None = None) -> None:
        span.ended_at = time.time()
        span.error = error
        if metadata:
            span.metadata.update(metadata)

    def get_trace(self, trace_id: str) -> list[TraceSpan]:
        return self._traces.get(trace_id, [])

    def flush_trace(self, trace_id: str) -> None:
        spans = self._traces.pop(trace_id, [])
        if self._logs_dir and spans:
            path = self._logs_dir / f"trace_{trace_id}.json"
            data = [s.to_dict() for s in spans]
            path.write_text(json.dumps(data, indent=2), encoding="utf-8")

    def get_recent_traces(self, limit: int = 20) -> list[str]:
        return list(self._traces.keys())[-limit:]


# ── Task Observer ────────────────────────────────────────────────────────────

@dataclass
class TaskView:
    task_id: str
    title: str
    status: str
    parent_id: str | None = None
    subtasks: list[TaskView] = field(default_factory=list)
    steps: list[str] = field(default_factory=list)
    error: str = ""
    retry_count: int = 0
    started_at: str = ""
    completed_at: str = ""
    duration_ms: int = 0


class TaskObserver:
    """Provides task-level observability beyond chat logs."""

    def __init__(self):
        self._views: dict[str, TaskView] = {}
        self._step_log: dict[str, list[str]] = {}

    def register_task(self, task_id: str, title: str, parent_id: str | None = None) -> None:
        view = TaskView(task_id=task_id, title=title, status="pending", parent_id=parent_id)
        self._views[task_id] = view
        if parent_id and parent_id in self._views:
            self._views[parent_id].subtasks.append(view)

    def update_status(self, task_id: str, status: str, error: str = "") -> None:
        view = self._views.get(task_id)
        if view:
            view.status = status
            view.error = error
            if status == "running" and not view.started_at:
                view.started_at = datetime.now().isoformat()
            if status in ("success", "failed", "cancelled"):
                view.completed_at = datetime.now().isoformat()

    def add_step(self, task_id: str, step: str) -> None:
        view = self._views.get(task_id)
        if view:
            view.steps.append(f"[{datetime.now().strftime('%H:%M:%S')}] {step}")

    def record_retry(self, task_id: str) -> None:
        view = self._views.get(task_id)
        if view:
            view.retry_count += 1

    def get_view(self, task_id: str) -> TaskView | None:
        return self._views.get(task_id)

    def get_all_views(self) -> list[TaskView]:
        return [v for v in self._views.values() if v.parent_id is None]

    def get_summary(self) -> dict[str, int]:
        statuses: dict[str, int] = {}
        for v in self._views.values():
            statuses[v.status] = statuses.get(v.status, 0) + 1
        return statuses


# ── Health Checker ───────────────────────────────────────────────────────────

class ComponentHealth(str, Enum):
    HEALTHY = "healthy"
    DEGRADED = "degraded"
    UNHEALTHY = "unhealthy"
    UNKNOWN = "unknown"


@dataclass
class HealthStatus:
    component: str
    status: ComponentHealth = ComponentHealth.UNKNOWN
    last_check: str = ""
    message: str = ""
    metadata: dict[str, Any] = field(default_factory=dict)


class HealthChecker:
    """Monitors system health and supports self-recovery."""

    def __init__(self, check_interval: int = 60):
        self._interval = check_interval
        self._components: dict[str, HealthStatus] = {}
        self._checks: dict[str, Callable[[], Awaitable[ComponentHealth]]] = {}
        self._running = False
        self._task: asyncio.Task | None = None
        self._recovery_handlers: dict[str, Callable[[], Awaitable[None]]] = {}

    def register_check(self, name: str, check_fn: Callable[[], Awaitable[ComponentHealth]], recovery_fn: Callable[[], Awaitable[None]] | None = None) -> None:
        self._checks[name] = check_fn
        self._components[name] = HealthStatus(component=name)
        if recovery_fn:
            self._recovery_handlers[name] = recovery_fn

    async def start(self) -> None:
        self._running = True
        self._task = asyncio.create_task(self._check_loop())
        logger.info("Health checker started")

    async def stop(self) -> None:
        self._running = False
        if self._task:
            self._task.cancel()
            try:
                await self._task
            except asyncio.CancelledError:
                pass

    async def check_all(self) -> dict[str, HealthStatus]:
        for name, check_fn in self._checks.items():
            try:
                status = await check_fn()
                self._components[name].status = status
                self._components[name].last_check = datetime.now().isoformat()
                self._components[name].message = ""

                if status == ComponentHealth.UNHEALTHY and name in self._recovery_handlers:
                    logger.warning("Component {} unhealthy, attempting recovery", name)
                    try:
                        await self._recovery_handlers[name]()
                        self._components[name].message = "recovery attempted"
                    except Exception as e:
                        self._components[name].message = f"recovery failed: {e}"
            except Exception as e:
                self._components[name].status = ComponentHealth.UNHEALTHY
                self._components[name].message = str(e)
                self._components[name].last_check = datetime.now().isoformat()
        return dict(self._components)

    def get_status(self) -> dict[str, Any]:
        overall = ComponentHealth.HEALTHY
        for hs in self._components.values():
            if hs.status == ComponentHealth.UNHEALTHY:
                overall = ComponentHealth.UNHEALTHY
                break
            if hs.status == ComponentHealth.DEGRADED:
                overall = ComponentHealth.DEGRADED
        return {
            "overall": overall.value,
            "components": {name: {"status": hs.status.value, "message": hs.message, "last_check": hs.last_check} for name, hs in self._components.items()},
        }

    async def _check_loop(self) -> None:
        while self._running:
            try:
                await self.check_all()
                await asyncio.sleep(self._interval)
            except asyncio.CancelledError:
                break
            except Exception as e:
                logger.error("Health check loop error: {}", e)
                await asyncio.sleep(self._interval)
