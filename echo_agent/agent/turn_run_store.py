"""Durable, authoritative lifecycle records for agent turns."""

from __future__ import annotations

import json
from datetime import datetime
from typing import Any


TERMINAL_TURN_STATUSES = frozenset({"completed", "incomplete", "failed", "interrupted"})


class TurnRunStore:
    """SQLite-backed turn ledger keyed by the inbound event id.

    The TUI's registry remains useful for live correlation, but it disappears on
    disconnect.  This store is the server-side source of truth used for status
    queries and reconnect reconciliation.
    """

    _MAX_RUNS_PER_SESSION = 500

    def __init__(self, storage: Any):
        self._storage = storage

    @staticmethod
    def _now() -> str:
        return datetime.now().isoformat()

    async def accept(
        self,
        event_id: str,
        session_key: str,
        *,
        context_key: str = "",
        trace_id: str = "",
        metadata: dict[str, Any] | None = None,
    ) -> None:
        now = self._now()
        await self._storage.execute_sql(
            "INSERT INTO turn_runs "
            "(event_id, session_key, context_key, trace_id, status, current_tool, "
            "response_text, error, metadata, created_at, started_at, updated_at, completed_at) "
            "VALUES (?, ?, ?, ?, 'accepted', '', '', '', ?, ?, '', ?, '') "
            "ON CONFLICT(event_id) DO UPDATE SET "
            "session_key=excluded.session_key, context_key=CASE WHEN excluded.context_key != '' "
            "THEN excluded.context_key ELSE turn_runs.context_key END, "
            "trace_id=CASE WHEN excluded.trace_id != '' THEN excluded.trace_id ELSE turn_runs.trace_id END, "
            "metadata=CASE WHEN excluded.metadata != '{}' THEN excluded.metadata ELSE turn_runs.metadata END, "
            "updated_at=excluded.updated_at",
            (
                event_id, session_key, context_key, trace_id,
                json.dumps(metadata or {}, ensure_ascii=False), now, now,
            ),
        )
        # The ledger is operational reconciliation state, not an infinite chat
        # archive. Bound each stable session independently so a years-long
        # cli:local identity cannot grow one SQLite row (and response body) per
        # turn forever. Cleanup is best-effort; acceptance itself already
        # committed and must remain usable if an older/custom backend rejects
        # DELETE ... OFFSET syntax.
        try:
            await self._storage.execute_sql(
                "DELETE FROM turn_runs WHERE event_id IN ("
                "SELECT event_id FROM turn_runs WHERE session_key=? "
                "ORDER BY created_at DESC LIMIT -1 OFFSET ?)",
                (session_key, self._MAX_RUNS_PER_SESSION),
            )
        except Exception:
            pass

    async def mark_running(
        self, event_id: str, session_key: str, *, context_key: str, trace_id: str,
    ) -> None:
        await self.accept(
            event_id, session_key, context_key=context_key, trace_id=trace_id,
        )
        now = self._now()
        await self._storage.execute_sql(
            "UPDATE turn_runs SET status='running', trace_id=?, context_key=?, "
            "started_at=CASE WHEN started_at='' THEN ? ELSE started_at END, updated_at=? "
            "WHERE event_id=? AND status NOT IN ('completed','incomplete','failed','interrupted')",
            (trace_id, context_key, now, now, event_id),
        )

    async def mark_activity(
        self, event_id: str, *, status: str = "running", current_tool: str = "",
    ) -> None:
        if status in TERMINAL_TURN_STATUSES:
            raise ValueError("mark_activity cannot write a terminal status")
        await self._storage.execute_sql(
            "UPDATE turn_runs SET status=?, current_tool=?, updated_at=? "
            "WHERE event_id=? AND status NOT IN ('completed','incomplete','failed','interrupted')",
            (status, current_tool, self._now(), event_id),
        )

    async def mark_terminal(
        self,
        event_id: str,
        status: str,
        *,
        response_text: str = "",
        error: str = "",
    ) -> None:
        if status not in TERMINAL_TURN_STATUSES:
            raise ValueError(f"invalid terminal turn status: {status}")
        now = self._now()
        await self._storage.execute_sql(
            "UPDATE turn_runs SET status=?, current_tool='', response_text=?, error=?, "
            "updated_at=?, completed_at=? WHERE event_id=? "
            "AND status NOT IN ('completed','incomplete','failed','interrupted')",
            (status, response_text, error, now, now, event_id),
        )

    async def get(self, event_id: str) -> dict[str, Any] | None:
        rows = await self._storage.fetch_sql(
            "SELECT * FROM turn_runs WHERE event_id=?", (event_id,),
        )
        return self._decode(rows[0]) if rows else None

    async def list_session(self, session_key: str, *, limit: int = 20) -> list[dict[str, Any]]:
        rows = await self._storage.fetch_sql(
            "SELECT * FROM turn_runs WHERE session_key=? "
            "ORDER BY created_at DESC LIMIT ?",
            (session_key, max(1, min(int(limit), 100))),
        )
        return [self._decode(row) for row in rows]

    async def latest(self, session_key: str) -> dict[str, Any] | None:
        rows = await self.list_session(session_key, limit=1)
        return rows[0] if rows else None

    @staticmethod
    def _decode(row: dict[str, Any]) -> dict[str, Any]:
        out = dict(row)
        raw = out.get("metadata") or "{}"
        if isinstance(raw, str):
            try:
                out["metadata"] = json.loads(raw)
            except json.JSONDecodeError:
                out["metadata"] = {}
        return out
