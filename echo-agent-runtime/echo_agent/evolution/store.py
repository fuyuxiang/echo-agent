"""TrajectoryStore — SQLite-backed persistence for the evolution subsystem.

Adds three tables to the shared SQLite backend (created via execute_sql at
init time, so it does not fight the global migration list in storage.sqlite):

  - evolution_trajectories
  - evolution_candidates
  - evolution_runs
"""

from __future__ import annotations

import json
from datetime import datetime, timedelta
from typing import Any

from loguru import logger

from echo_agent.evolution.types import (
    CandidateStatus,
    EvolutionRun,
    Outcome,
    SkillCandidate,
    Trajectory,
)
from echo_agent.storage.backend import StorageBackend


_SCHEMA_STATEMENTS: list[str] = [
    """CREATE TABLE IF NOT EXISTS evolution_trajectories (
        id TEXT PRIMARY KEY,
        session_id TEXT NOT NULL DEFAULT '',
        chat_id TEXT NOT NULL DEFAULT '',
        channel TEXT NOT NULL DEFAULT '',
        task_type TEXT NOT NULL DEFAULT '',
        outcome TEXT NOT NULL DEFAULT 'success',
        score REAL,
        iterations INTEGER NOT NULL DEFAULT 0,
        duration_ms REAL NOT NULL DEFAULT 0,
        data TEXT NOT NULL,
        created_at TEXT NOT NULL,
        consumed_run_id TEXT NOT NULL DEFAULT ''
    )""",
    "CREATE INDEX IF NOT EXISTS idx_evolution_traj_outcome "
    "ON evolution_trajectories(outcome, created_at)",
    "CREATE INDEX IF NOT EXISTS idx_evolution_traj_task_type "
    "ON evolution_trajectories(task_type, outcome)",
    "CREATE INDEX IF NOT EXISTS idx_evolution_traj_consumed "
    "ON evolution_trajectories(consumed_run_id)",
    """CREATE TABLE IF NOT EXISTS evolution_candidates (
        id TEXT PRIMARY KEY,
        run_id TEXT NOT NULL DEFAULT '',
        operation TEXT NOT NULL DEFAULT 'create',
        skill_name TEXT NOT NULL DEFAULT '',
        status TEXT NOT NULL DEFAULT 'pending',
        data TEXT NOT NULL,
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL
    )""",
    "CREATE INDEX IF NOT EXISTS idx_evolution_cand_status "
    "ON evolution_candidates(status)",
    "CREATE INDEX IF NOT EXISTS idx_evolution_cand_run "
    "ON evolution_candidates(run_id)",
    "CREATE INDEX IF NOT EXISTS idx_evolution_cand_skill "
    "ON evolution_candidates(skill_name)",
    """CREATE TABLE IF NOT EXISTS evolution_runs (
        id TEXT PRIMARY KEY,
        triggered_by TEXT NOT NULL DEFAULT 'manual',
        data TEXT NOT NULL,
        started_at TEXT NOT NULL,
        finished_at TEXT NOT NULL DEFAULT ''
    )""",
    "CREATE INDEX IF NOT EXISTS idx_evolution_runs_started "
    "ON evolution_runs(started_at)",
]


class TrajectoryStore:
    """Persistence layer for trajectories, candidates, and evolution runs.

    Designed to be safe to call concurrently from background tasks: every
    method opens a single execute/fetch round-trip on the shared backend.
    """

    def __init__(self, storage: StorageBackend):
        self._storage = storage
        self._initialized = False

    async def init_schema(self) -> None:
        if self._initialized:
            return
        for stmt in _SCHEMA_STATEMENTS:
            try:
                await self._storage.execute_sql(stmt)
            except Exception as e:
                logger.error("Failed to apply evolution schema statement: {}", e)
                raise
        self._initialized = True

    # ── Trajectories ────────────────────────────────────────────────────────

    async def append_trajectory(self, trajectory: Trajectory) -> None:
        payload = json.dumps(trajectory.to_dict(), ensure_ascii=False)
        await self._storage.execute_sql(
            "INSERT OR REPLACE INTO evolution_trajectories "
            "(id, session_id, chat_id, channel, task_type, outcome, score, "
            " iterations, duration_ms, data, created_at, consumed_run_id) "
            "VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, "
            "        COALESCE((SELECT consumed_run_id FROM evolution_trajectories WHERE id = ?), ''))",
            (
                trajectory.id,
                trajectory.session_id,
                trajectory.chat_id,
                trajectory.channel,
                trajectory.task_type,
                trajectory.outcome,
                trajectory.reflection_score,
                trajectory.iterations,
                trajectory.duration_ms,
                payload,
                trajectory.created_at,
                trajectory.id,
            ),
        )

    async def get_trajectory(self, trajectory_id: str) -> Trajectory | None:
        rows = await self._storage.fetch_sql(
            "SELECT data FROM evolution_trajectories WHERE id = ?",
            (trajectory_id,),
        )
        if not rows:
            return None
        return Trajectory.from_dict(json.loads(rows[0]["data"]))

    async def list_trajectories(
        self,
        *,
        limit: int = 200,
        outcome: Outcome | None = None,
        task_type: str | None = None,
        only_unconsumed: bool = False,
        since: str | None = None,
    ) -> list[Trajectory]:
        clauses: list[str] = []
        params: list[Any] = []
        if outcome:
            clauses.append("outcome = ?")
            params.append(outcome)
        if task_type:
            clauses.append("task_type = ?")
            params.append(task_type)
        if only_unconsumed:
            clauses.append("consumed_run_id = ''")
        if since:
            clauses.append("created_at >= ?")
            params.append(since)
        where = ""
        if clauses:
            where = " WHERE " + " AND ".join(clauses)
        params.append(int(limit))
        rows = await self._storage.fetch_sql(
            f"SELECT data FROM evolution_trajectories{where} "
            f"ORDER BY created_at DESC LIMIT ?",
            tuple(params),
        )
        out: list[Trajectory] = []
        for r in rows:
            try:
                out.append(Trajectory.from_dict(json.loads(r["data"])))
            except Exception as e:
                logger.debug("Skipping malformed trajectory row: {}", e)
        return out

    async def count_unconsumed(self) -> int:
        rows = await self._storage.fetch_sql(
            "SELECT COUNT(*) AS n FROM evolution_trajectories WHERE consumed_run_id = ''",
        )
        if not rows:
            return 0
        try:
            return int(rows[0].get("n", 0) or 0)
        except (TypeError, ValueError):
            return 0

    async def mark_consumed(self, trajectory_ids: list[str], run_id: str) -> None:
        if not trajectory_ids:
            return
        placeholders = ",".join(["?"] * len(trajectory_ids))
        await self._storage.execute_sql(
            f"UPDATE evolution_trajectories SET consumed_run_id = ? "
            f"WHERE id IN ({placeholders})",
            tuple([run_id, *trajectory_ids]),
        )

    async def purge_older_than(self, retention_days: int) -> int:
        if retention_days <= 0:
            return 0
        cutoff = (datetime.now() - timedelta(days=retention_days)).isoformat()
        before = await self._storage.fetch_sql(
            "SELECT COUNT(*) AS n FROM evolution_trajectories WHERE created_at < ?",
            (cutoff,),
        )
        try:
            count = int(before[0].get("n", 0) or 0) if before else 0
        except (TypeError, ValueError):
            count = 0
        await self._storage.execute_sql(
            "DELETE FROM evolution_trajectories WHERE created_at < ?",
            (cutoff,),
        )
        return count

    # ── Candidates ──────────────────────────────────────────────────────────

    async def save_candidate(self, candidate: SkillCandidate) -> None:
        now = datetime.now().isoformat()
        payload = json.dumps(candidate.to_dict(), ensure_ascii=False)
        await self._storage.execute_sql(
            "INSERT OR REPLACE INTO evolution_candidates "
            "(id, run_id, operation, skill_name, status, data, created_at, updated_at) "
            "VALUES (?, ?, ?, ?, ?, ?, "
            "        COALESCE((SELECT created_at FROM evolution_candidates WHERE id = ?), ?), "
            "        ?)",
            (
                candidate.id,
                candidate.run_id,
                candidate.operation,
                candidate.skill_name,
                candidate.status,
                payload,
                candidate.id,
                candidate.created_at or now,
                now,
            ),
        )

    async def get_candidate(self, candidate_id: str) -> SkillCandidate | None:
        rows = await self._storage.fetch_sql(
            "SELECT data FROM evolution_candidates WHERE id = ?",
            (candidate_id,),
        )
        if not rows:
            return None
        return SkillCandidate.from_dict(json.loads(rows[0]["data"]))

    async def list_candidates(
        self,
        *,
        status: CandidateStatus | None = None,
        run_id: str | None = None,
        skill_name: str | None = None,
        limit: int = 100,
    ) -> list[SkillCandidate]:
        clauses: list[str] = []
        params: list[Any] = []
        if status:
            clauses.append("status = ?")
            params.append(status)
        if run_id:
            clauses.append("run_id = ?")
            params.append(run_id)
        if skill_name:
            clauses.append("skill_name = ?")
            params.append(skill_name)
        where = ""
        if clauses:
            where = " WHERE " + " AND ".join(clauses)
        params.append(int(limit))
        rows = await self._storage.fetch_sql(
            f"SELECT data FROM evolution_candidates{where} "
            f"ORDER BY updated_at DESC LIMIT ?",
            tuple(params),
        )
        out: list[SkillCandidate] = []
        for r in rows:
            try:
                out.append(SkillCandidate.from_dict(json.loads(r["data"])))
            except Exception as e:
                logger.debug("Skipping malformed candidate row: {}", e)
        return out

    async def update_candidate(self, candidate: SkillCandidate) -> None:
        await self.save_candidate(candidate)

    async def latest_promoted_for_skill(self, skill_name: str) -> SkillCandidate | None:
        rows = await self._storage.fetch_sql(
            "SELECT data FROM evolution_candidates "
            "WHERE skill_name = ? AND status = 'promoted' "
            "ORDER BY updated_at DESC LIMIT 1",
            (skill_name,),
        )
        if not rows:
            return None
        return SkillCandidate.from_dict(json.loads(rows[0]["data"]))

    # ── Runs ────────────────────────────────────────────────────────────────

    async def save_run(self, run: EvolutionRun) -> None:
        payload = json.dumps(run.to_dict(), ensure_ascii=False)
        await self._storage.execute_sql(
            "INSERT OR REPLACE INTO evolution_runs "
            "(id, triggered_by, data, started_at, finished_at) "
            "VALUES (?, ?, ?, ?, ?)",
            (run.id, run.triggered_by, payload, run.started_at, run.finished_at),
        )

    async def get_run(self, run_id: str) -> EvolutionRun | None:
        rows = await self._storage.fetch_sql(
            "SELECT data FROM evolution_runs WHERE id = ?",
            (run_id,),
        )
        if not rows:
            return None
        return EvolutionRun.from_dict(json.loads(rows[0]["data"]))

    async def list_runs(self, *, limit: int = 25) -> list[EvolutionRun]:
        rows = await self._storage.fetch_sql(
            "SELECT data FROM evolution_runs ORDER BY started_at DESC LIMIT ?",
            (int(limit),),
        )
        out: list[EvolutionRun] = []
        for r in rows:
            try:
                out.append(EvolutionRun.from_dict(json.loads(r["data"])))
            except Exception as e:
                logger.debug("Skipping malformed run row: {}", e)
        return out

    async def latest_run(self) -> EvolutionRun | None:
        runs = await self.list_runs(limit=1)
        return runs[0] if runs else None
