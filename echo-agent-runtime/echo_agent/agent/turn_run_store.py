"""Durable, authoritative lifecycle records for agent turns."""

from __future__ import annotations

import json
import time
from collections.abc import Awaitable, Callable
from datetime import datetime
from typing import Any


TERMINAL_TURN_STATUSES = frozenset({"completed", "incomplete", "failed", "interrupted"})


class DuplicateTurnClaim(Exception):
    """Internal control signal: this event ID was already claimed by a turn."""


class TurnRunStore:
    """SQLite-backed turn ledger keyed by the inbound event id.

    The TUI's registry remains useful for live correlation, but it disappears on
    disconnect.  This store is the server-side source of truth used for status
    queries and reconnect reconciliation.
    """

    _MAX_RUNS_PER_SESSION = 500
    _IDEMPOTENCY_TTL_SECONDS = 3600.0
    _MAX_IDEMPOTENCY_RECORDS = 100_000
    _MAX_OUTBOUND_PARTS = 100_000

    def __init__(
        self,
        storage: Any,
        *,
        clock: Callable[[], float] = time.time,
    ) -> None:
        self._storage = storage
        self._clock = clock
        self._event_sink: Callable[[str, dict[str, Any]], Awaitable[None]] | None = None

    def set_event_sink(
        self,
        sink: Callable[[str, dict[str, Any]], Awaitable[None]] | None,
    ) -> None:
        self._event_sink = sink

    async def get_outbound_part(self, part_id: str) -> dict[str, Any] | None:
        rows = await self._storage.fetch_sql("SELECT * FROM outbound_parts WHERE part_id=?", (part_id,))
        return dict(rows[0]) if rows else None

    async def claim_outbound_part(self, part_id: str, fingerprint: str) -> dict[str, Any]:
        """Reserve before sending; an empty receipt remains ambiguous after a crash.

        Never evict delivery identities to make space: that would replay an
        acknowledged or uncertain send. Only a definite rejection is releasable.
        """
        changed = await self._storage.execute_sql(
            "INSERT INTO outbound_parts(part_id,fingerprint,state,updated_at) SELECT ?,?,'inflight',? "
            "WHERE (SELECT COUNT(*) FROM outbound_parts) < ? "
            "ON CONFLICT(part_id) DO NOTHING",
            (part_id, fingerprint, self._clock(), self._MAX_OUTBOUND_PARTS),
        )
        if changed is None:
            raise RuntimeError("outbound storage cannot arbitrate delivery")
        row = await self.get_outbound_part(part_id)
        if row is None:
            raise RuntimeError("outbound delivery ledger is full")
        if row["fingerprint"] != fingerprint:
            raise RuntimeError("outbound delivery identity conflict")
        return {**row, "new": changed == 1}

    async def finish_outbound_part(self, part_id: str, message_id: str | None) -> None:
        """Persist a platform receipt, or release a definitively rejected request."""
        if message_id is None:
            await self._storage.execute_sql(
                "DELETE FROM outbound_parts WHERE part_id=? AND message_id=''", (part_id,),
            )
        else:
            changed = await self._storage.execute_sql(
                "UPDATE outbound_parts SET message_id=?,state='delivered',updated_at=? WHERE part_id=? AND message_id=''",
                (message_id, self._clock(), part_id),
            )
            if changed != 1:
                raise RuntimeError("outbound receipt was not persisted")

    async def mark_outbound_unknown(self, part_id: str) -> None:
        await self._storage.execute_sql(
            "UPDATE outbound_parts SET state='unknown',updated_at=? WHERE part_id=? AND message_id=''",
            (self._clock(), part_id),
        )

    async def register_outbound(self, event) -> None:
        from dataclasses import asdict
        values = {
            "event_id": event.event_id, "channel": event.channel, "chat_id": event.chat_id,
            "content": [asdict(block) for block in event.content], "reply_to_id": event.reply_to_id,
            "metadata": {key: value for key, value in event.metadata.items()
                         if not key.startswith("_") or key in {"_inbound_event_id", "_clarification"}},
            "message_kind": event.message_kind,
        }
        if event.delivery_origin is not None:
            origin = asdict(event.delivery_origin)
            origin["timestamp"] = event.delivery_origin.timestamp.isoformat()
            values["delivery_origin"] = origin
        payload = json.dumps(values, ensure_ascii=False, sort_keys=True)
        await self._storage.execute_sql(
            "INSERT INTO outbound_messages(event_id,payload,created_at,updated_at) SELECT ?,?,?,? "
            "WHERE (SELECT COUNT(*) FROM outbound_messages) < ? "
            "ON CONFLICT(event_id) DO NOTHING",
            (event.event_id, payload, self._clock(), self._clock(), self._MAX_OUTBOUND_PARTS),
        )
        rows = await self._storage.fetch_sql("SELECT payload FROM outbound_messages WHERE event_id=?", (event.event_id,))
        if not rows:
            raise RuntimeError("outbound recovery ledger is full")
        if rows[0]["payload"] != payload:
            raise RuntimeError("outbound delivery identity conflict")

    async def update_outbound(self, event_id: str, status: str, error: str = "") -> None:
        await self._storage.execute_sql(
            "UPDATE outbound_messages SET status=?,error=?,updated_at=? WHERE event_id=?",
            (status, error, self._clock(), event_id),
        )

    async def list_outbound(self, *, limit: int = 100) -> list[dict]:
        rows = await self._storage.fetch_sql(
            "SELECT * FROM outbound_messages WHERE status!='delivered' ORDER BY created_at DESC LIMIT ?",
            (max(1, min(limit, 100)),),
        )
        result = []
        for row in rows:
            payload = json.loads(row["payload"])
            parts = await self.outbound_parts(row["event_id"], payload["channel"])
            result.append({**row, "payload": payload, "parts": parts})
        return result

    async def outbound_parts(self, event_id: str, channel: str) -> list[dict]:
        prefix = f"{channel}:{event_id}:"
        escaped = prefix.replace("\\", "\\\\").replace("%", "\\%").replace("_", "\\_")
        return await self._storage.fetch_sql(
            "SELECT part_id,state,message_id FROM outbound_parts WHERE part_id LIKE ? ESCAPE '\\'",
            (escaped + "%",),
        )

    async def get_outbound_event(self, event_id: str):
        from datetime import datetime
        from echo_agent.bus.events import ContentBlock, ContentType, EventType, InboundEvent, OutboundEvent
        rows = await self._storage.fetch_sql("SELECT payload FROM outbound_messages WHERE event_id=?", (event_id,))
        if not rows:
            return None
        payload = json.loads(rows[0]["payload"])
        payload["content"] = [ContentBlock(**{**block, "type": ContentType(block["type"])}) for block in payload["content"]]
        origin = payload.get("delivery_origin")
        if origin is not None:
            origin["timestamp"] = datetime.fromisoformat(origin["timestamp"])
            origin["event_type"] = EventType(origin["event_type"])
            origin["content"] = [ContentBlock(**{**block, "type": ContentType(block["type"])}) for block in origin["content"]]
            payload["delivery_origin"] = InboundEvent(**origin)
        return OutboundEvent(**payload)

    async def resolve_outbound(self, event_id: str, *, confirmed_not_delivered: bool = False,
                               message_ids: dict[str, str] | None = None, actor: str = "operator") -> None:
        event = await self.get_outbound_event(event_id)
        if event is None:
            raise ValueError("outbound message not found")
        pending = [row for row in await self.outbound_parts(event_id, event.channel) if not row["message_id"]]
        if any(row["state"] == "inflight" for row in pending):
            raise ValueError("delivery is still in progress")
        if pending and not confirmed_not_delivered and not message_ids:
            raise ValueError("confirm the platform outcome before retrying an unknown delivery")
        for row in pending:
            receipt = (message_ids or {}).get(row["part_id"])
            if receipt is not None and (not isinstance(receipt, str) or not receipt.strip() or len(receipt) > 500):
                raise ValueError("a non-empty platform receipt is required")
            if receipt is None and not confirmed_not_delivered:
                raise ValueError("a platform receipt is required for every unknown part")
        for row in pending:
            receipt = (message_ids or {}).get(row["part_id"])
            await self._storage.execute_sql(
                "INSERT INTO outbound_resolutions(part_id,action,message_id,resolved_at,actor) VALUES (?,?,?,?,?)",
                (row["part_id"], "confirmed_delivered" if receipt else "confirmed_not_delivered", receipt or "", self._clock(), actor),
            )
            await self.finish_outbound_part(row["part_id"], receipt)

    async def _emit(self, event_id: str) -> None:
        if self._event_sink is None:
            return
        try:
            row = await self.get(event_id)
            if row is not None:
                await self._event_sink("session_turn_updated", row)
        except Exception:
            # Dashboard notification is best-effort; durable turn state was
            # already written and must not fail because an observer is gone.
            pass

    @staticmethod
    def _now() -> str:
        return datetime.now().isoformat()

    async def _prune_terminal_session(self, session_key: str) -> None:
        """Keep recent results bounded without ever deleting live authority."""
        try:
            await self._storage.execute_sql(
                "DELETE FROM turn_runs WHERE event_id IN ("
                "SELECT event_id FROM turn_runs WHERE session_key=? "
                "AND status IN ('completed','incomplete','failed','interrupted') "
                "ORDER BY created_at DESC, rowid DESC LIMIT -1 OFFSET ?)",
                (session_key, self._MAX_RUNS_PER_SESSION),
            )
        except Exception:
            # Retention is best-effort. A backend that lacks SQLite's OFFSET or
            # rowid semantics must not turn lifecycle observability into an
            # ingestion failure.
            pass

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
            # Acceptance is create-only. A duplicate delivery must not rewrite
            # ownership/session metadata on the authoritative first record, and
            # must never reset a running/terminal row back toward execution.
            "ON CONFLICT(event_id) DO NOTHING",
            (
                event_id,
                session_key,
                context_key,
                trace_id,
                json.dumps(metadata or {}, ensure_ascii=False),
                now,
                now,
            ),
        )
        # Bound cached terminal results, but never delete accepted/running work:
        # those rows are the at-most-once authority and may legitimately exceed
        # the result retention bound during a burst or long approval wait.
        await self._prune_terminal_session(session_key)
        await self._emit(event_id)

    async def claim_idempotency(
        self,
        event_id: str,
        *,
        namespace: str,
        fingerprint: str,
        session_key: str,
    ) -> dict[str, Any]:
        """Atomically persist a restart-safe idempotency tombstone.

        This table is intentionally independent of the 500-row per-session turn
        result cache. Records live for the full idempotency TTL even if the rich
        turn row is pruned, and the hard capacity fails closed instead of
        evicting an unexpired key that could then execute twice.
        """
        now_epoch = self._clock()
        now = self._now()
        await self._storage.execute_sql(
            "DELETE FROM inbound_idempotency WHERE expires_at<=?",
            (now_epoch,),
        )
        changed = await self._storage.execute_sql(
            "INSERT INTO inbound_idempotency "
            "(event_id, namespace, fingerprint, session_key, status, "
            "response_text, error, created_at, updated_at, expires_at) "
            "SELECT ?, ?, ?, ?, 'pending', '', '', ?, ?, ? "
            "WHERE (SELECT COUNT(*) FROM inbound_idempotency) < ? "
            "ON CONFLICT(event_id) DO NOTHING",
            (
                event_id,
                namespace,
                fingerprint,
                session_key,
                now,
                now,
                now_epoch + self._IDEMPOTENCY_TTL_SECONDS,
                self._MAX_IDEMPOTENCY_RECORDS,
            ),
        )
        if changed is None:
            # Affected-row reporting is required to distinguish our insert from
            # an existing claim under concurrency. Unlike turn observability,
            # restart-safe idempotency cannot safely fail open.
            raise RuntimeError("storage backend cannot arbitrate idempotency claims")
        row = await self.get_idempotency(event_id)
        if row is None:
            return {"outcome": "full"}
        if (
            row.get("namespace") != namespace
            or row.get("fingerprint") != fingerprint
            or row.get("session_key") != session_key
        ):
            return {"outcome": "conflict", "row": row}
        return {"outcome": "new" if changed == 1 else "duplicate", "row": row}

    async def get_idempotency(self, event_id: str) -> dict[str, Any] | None:
        rows = await self._storage.fetch_sql(
            "SELECT * FROM inbound_idempotency WHERE event_id=? AND expires_at>?",
            (event_id, self._clock()),
        )
        return dict(rows[0]) if rows else None

    async def mark_idempotency_admitted(self, event_id: str) -> None:
        await self._storage.execute_sql(
            "UPDATE inbound_idempotency SET status='accepted', updated_at=? WHERE event_id=? AND status='pending'",
            (self._now(), event_id),
        )

    async def release_idempotency(self, event_id: str) -> bool:
        changed = await self._storage.execute_sql(
            "DELETE FROM inbound_idempotency WHERE event_id=? AND status IN ('pending','accepted')",
            (event_id,),
        )
        if changed is None:
            raise RuntimeError("storage backend cannot confirm idempotency release")
        return changed == 1

    async def sync_idempotency_from_turn(self, row: dict[str, Any]) -> None:
        """Copy authoritative turn progress/result into an existing tombstone."""
        status = str(row.get("status") or "accepted")
        response_text = str(row.get("response_text") or "")
        error = str(row.get("error") or "")
        terminal = status in TERMINAL_TURN_STATUSES
        expires_at = self._clock() + self._IDEMPOTENCY_TTL_SECONDS if terminal else None
        if expires_at is None:
            await self._storage.execute_sql(
                "UPDATE inbound_idempotency SET status=?, response_text=?, error=?, updated_at=? WHERE event_id=?",
                (status, response_text, error, self._now(), row.get("event_id")),
            )
        else:
            await self._storage.execute_sql(
                "UPDATE inbound_idempotency SET status=?, response_text=?, "
                "error=?, updated_at=?, expires_at=? WHERE event_id=?",
                (
                    status,
                    response_text,
                    error,
                    self._now(),
                    expires_at,
                    row.get("event_id"),
                ),
            )

    async def mark_running(
        self,
        event_id: str,
        session_key: str,
        *,
        context_key: str,
        trace_id: str,
        metadata: dict[str, Any] | None = None,
    ) -> bool:
        """Atomically claim an accepted event for exactly one executing turn."""
        await self.accept(
            event_id,
            session_key,
            context_key=context_key,
            trace_id=trace_id,
            metadata=metadata,
        )
        now = self._now()
        changed = await self._storage.execute_sql(
            "UPDATE turn_runs SET status='running', trace_id=?, context_key=?, "
            "started_at=CASE WHEN started_at='' THEN ? ELSE started_at END, updated_at=? "
            "WHERE event_id=? AND status='accepted'",
            (trace_id, context_key, now, now, event_id),
        )
        if changed is None:
            # Compatibility for third-party StorageBackend implementations that
            # predate affected-row reporting. Built-in SQLite always returns an
            # integer and therefore provides the atomic claim guarantee.
            return True
        claimed = changed == 1
        if claimed:
            await self._storage.execute_sql(
                "UPDATE inbound_idempotency SET status='running', updated_at=? "
                "WHERE event_id=? AND status IN ('pending','accepted')",
                (self._now(), event_id),
            )
            await self._emit(event_id)
        return claimed

    async def release_acceptance(self, event_id: str, session_key: str) -> bool:
        """Delete a pre-admission row only while it is still safely accepted.

        Ingress may write ``accepted`` before publishing to the bus. When the
        bus definitively returns ``False``, that event was not enqueued and the
        same deterministic event ID must be retryable. The status/session guard
        makes this safe under races: a running or terminal row is never reset.
        """
        changed = await self._storage.execute_sql(
            "DELETE FROM turn_runs WHERE event_id=? AND session_key=? AND status='accepted'",
            (event_id, session_key),
        )
        # Older third-party backends do not report rowcount, but still execute
        # the guarded DELETE. Built-in SQLite returns the authoritative count.
        return changed is None or changed == 1

    async def mark_activity(
        self,
        event_id: str,
        *,
        status: str = "running",
        current_tool: str = "",
    ) -> None:
        if status in TERMINAL_TURN_STATUSES:
            raise ValueError("mark_activity cannot write a terminal status")
        await self._storage.execute_sql(
            "UPDATE turn_runs SET status=?, current_tool=?, updated_at=? "
            "WHERE event_id=? AND status NOT IN ('completed','incomplete','failed','interrupted')",
            (status, current_tool, self._now(), event_id),
        )
        await self._emit(event_id)

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
        changed = await self._storage.execute_sql(
            "UPDATE turn_runs SET status=?, current_tool='', response_text=?, error=?, "
            "updated_at=?, completed_at=? WHERE event_id=? "
            "AND status NOT IN ('completed','incomplete','failed','interrupted')",
            (status, response_text, error, now, now, event_id),
        )
        row = await self.get(event_id)
        if changed == 0 and row is not None:
            # The turn was already terminal. Mirror that authoritative first
            # result instead of letting a late duplicate callback rewrite only
            # the tombstone and make replay disagree with turn status.
            await self.sync_idempotency_from_turn(row)
        else:
            # A bus-level rejection can legitimately settle a keyed event
            # before an agent turn row exists, so the tombstone is independently
            # terminalizable. Its own guard preserves first-terminal-wins.
            await self._storage.execute_sql(
                "UPDATE inbound_idempotency SET status=?, response_text=?, error=?, "
                "updated_at=?, expires_at=? WHERE event_id=? "
                "AND status NOT IN ('completed','incomplete','failed','interrupted')",
                (
                    status,
                    response_text,
                    error,
                    now,
                    self._clock() + self._IDEMPOTENCY_TTL_SECONDS,
                    event_id,
                ),
            )
        if row is not None:
            await self._prune_terminal_session(str(row.get("session_key") or ""))
        await self._emit(event_id)

    async def reconcile_orphaned(self, *, error: str = "process restarted") -> int:
        """Fail every non-terminal row left by a previous process instance.

        This runs before AgentLoop subscribes to inbound traffic, so there are no
        current-instance rows to race with.  It intentionally includes
        ``accepted`` as well as ``running``/waiting states: a crash after Gateway
        acceptance but before dispatch leaves the same false "still running"
        experience on reconnect.
        """
        await self._storage.execute_sql(
            "UPDATE outbound_parts SET state='unknown',updated_at=? WHERE state='inflight' AND message_id=''",
            (self._clock(),),
        )
        await self._storage.execute_sql(
            "UPDATE outbound_messages SET status='unknown',updated_at=? WHERE status IN ('pending','retrying')",
            (self._clock(),),
        )
        rows = await self._storage.fetch_sql(
            "SELECT COUNT(*) AS count FROM turn_runs "
            "WHERE status NOT IN ('completed','incomplete','failed','interrupted')",
        )
        count = int(rows[0].get("count", 0)) if rows else 0
        if not count:
            return 0

        sessions = await self._storage.fetch_sql(
            "SELECT DISTINCT session_key FROM turn_runs "
            "WHERE status NOT IN ('completed','incomplete','failed','interrupted')",
        )

        now = self._now()
        await self._storage.execute_sql(
            "UPDATE turn_runs SET status='failed', current_tool='', error=?, "
            "updated_at=?, completed_at=? "
            "WHERE status NOT IN ('completed','incomplete','failed','interrupted')",
            (error, now, now),
        )
        await self._storage.execute_sql(
            "UPDATE inbound_idempotency SET status='failed', error=?, "
            "updated_at=?, expires_at=? WHERE event_id IN ("
            "SELECT event_id FROM turn_runs WHERE status='failed' AND error=?) "
            "AND status NOT IN ('completed','incomplete','failed','interrupted')",
            (
                error,
                now,
                self._clock() + self._IDEMPOTENCY_TTL_SECONDS,
                error,
            ),
        )
        for row in sessions:
            await self._prune_terminal_session(str(row.get("session_key") or ""))
        return count

    async def get(self, event_id: str) -> dict[str, Any] | None:
        rows = await self._storage.fetch_sql(
            "SELECT * FROM turn_runs WHERE event_id=?",
            (event_id,),
        )
        return self._decode(rows[0]) if rows else None

    async def list_session(self, session_key: str, *, limit: int = 20) -> list[dict[str, Any]]:
        rows = await self._storage.fetch_sql(
            "SELECT * FROM turn_runs WHERE session_key=? ORDER BY created_at DESC LIMIT ?",
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
