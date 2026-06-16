"""PlanRunStore — durable persistence for multi-step plan execution.

Plans were previously request-scoped objects living only in PipelineContext:
the StepStatus fields existed but never flowed, nothing was written to storage,
and an interrupted long task left no trace of "which step were we on". This
store records each multi-step plan as a PlanRun row, updates step status as the
turn progresses, and exposes the latest run for a session so progress is
queryable and resumable.
"""

from __future__ import annotations

import uuid
from datetime import datetime
from typing import TYPE_CHECKING, Any

from loguru import logger

from echo_agent.agent.planning.models import Plan

if TYPE_CHECKING:
    from echo_agent.storage.backend import StorageBackend


class PlanRunStore:
    """SQLite-backed persistence for plan execution state."""

    def __init__(self, storage: "StorageBackend"):
        self._storage = storage

    async def create(self, session_key: str, trace_id: str, plan: Plan) -> str:
        """Persist a new plan run, returning its id."""
        import json

        run_id = uuid.uuid4().hex[:12]
        now = datetime.now().isoformat()
        await self._storage.execute_sql(
            "INSERT INTO plan_runs (id, session_key, trace_id, goal, strategy, "
            "status, current_step, plan, created_at, updated_at) "
            "VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            (
                run_id, session_key, trace_id, plan.goal, plan.strategy.value,
                "running", plan.current_step,
                json.dumps(plan.to_dict(), ensure_ascii=False), now, now,
            ),
        )
        logger.debug("Created plan run {} for session {}", run_id, session_key)
        return run_id

    async def update(self, run_id: str, plan: Plan, status: str | None = None) -> None:
        """Write back the current plan state (step status, current_step)."""
        import json

        resolved_status = status or ("complete" if plan.is_complete else "running")
        await self._storage.execute_sql(
            "UPDATE plan_runs SET status = ?, current_step = ?, plan = ?, "
            "updated_at = ? WHERE id = ?",
            (
                resolved_status, plan.current_step,
                json.dumps(plan.to_dict(), ensure_ascii=False),
                datetime.now().isoformat(), run_id,
            ),
        )

    async def get_latest(self, session_key: str) -> dict[str, Any] | None:
        """Return the most recent plan run for a session, or None."""
        rows = await self._storage.fetch_sql(
            "SELECT * FROM plan_runs WHERE session_key = ? "
            "ORDER BY created_at DESC LIMIT 1",
            (session_key,),
        )
        if not rows:
            return None
        return self._row_to_run(rows[0])

    async def get_resumable(self, session_key: str) -> Plan | None:
        """Return the latest still-running plan for a session as a Plan, so an
        interrupted multi-step task can be picked up where it left off."""
        run = await self.get_latest(session_key)
        if run is None or run.get("status") != "running":
            return None
        return run.get("plan")

    @staticmethod
    def _row_to_run(row: dict[str, Any]) -> dict[str, Any]:
        import json

        raw_plan = row.get("plan", "{}")
        if isinstance(raw_plan, str):
            try:
                raw_plan = json.loads(raw_plan)
            except json.JSONDecodeError:
                raw_plan = {}
        plan = Plan.from_dict(raw_plan) if raw_plan else None
        return {
            "id": row.get("id", ""),
            "session_key": row.get("session_key", ""),
            "trace_id": row.get("trace_id", ""),
            "goal": row.get("goal", ""),
            "strategy": row.get("strategy", ""),
            "status": row.get("status", ""),
            "current_step": row.get("current_step", 0),
            "plan": plan,
            "created_at": row.get("created_at", ""),
            "updated_at": row.get("updated_at", ""),
        }
