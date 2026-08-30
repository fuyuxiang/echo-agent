"""SQLite storage backend — async implementation with error recovery."""

from __future__ import annotations

import asyncio
import json
from contextlib import asynccontextmanager
from datetime import datetime
from pathlib import Path
from typing import Any, Awaitable

import aiosqlite
from loguru import logger

from echo_agent.storage.backend import StorageBackend
from echo_agent.storage.errors import CorruptData, StorageUnavailable


async def _settle_cleanup(operation: Awaitable[Any]) -> asyncio.CancelledError | None:
    """Await cleanup to completion while deferring repeated caller cancellation.

    ``asyncio.shield`` alone only prevents cancellation of the child task; the
    awaiting task still exits immediately on a second ``cancel()``, which can
    release a transaction/connection lock while rollback or close is running in
    the background.  This helper keeps the child task referenced and waits for
    its real terminal state.  The caller decides when to re-raise any deferred
    cancellation after its whole cleanup sequence is safe.
    """
    task = asyncio.ensure_future(operation)
    deferred: asyncio.CancelledError | None = None
    while not task.done():
        try:
            await asyncio.shield(task)
        except asyncio.CancelledError as error:
            if task.done() and task.cancelled():
                raise
            deferred = error
        except BaseException:
            # Retrieve the child exception below, preserving its traceback and
            # avoiding an unobserved-task warning.
            break
    task.result()
    return deferred


_SCHEMA_SQL = """\
CREATE TABLE IF NOT EXISTS sessions (
    key TEXT PRIMARY KEY,
    data TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS memories (
    id TEXT PRIMARY KEY,
    type TEXT NOT NULL,
    key TEXT,
    data TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_memories_type ON memories(type);
CREATE TABLE IF NOT EXISTS tasks (
    id TEXT PRIMARY KEY,
    workflow_id TEXT,
    status TEXT NOT NULL,
    data TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_tasks_workflow ON tasks(workflow_id);
CREATE INDEX IF NOT EXISTS idx_tasks_status ON tasks(status);
CREATE TABLE IF NOT EXISTS workflows (
    id TEXT PRIMARY KEY,
    name TEXT,
    status TEXT NOT NULL,
    data TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_workflows_status ON workflows(status);
CREATE TABLE IF NOT EXISTS logs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    trace_id TEXT NOT NULL,
    data TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_logs_trace ON logs(trace_id);
CREATE TABLE IF NOT EXISTS files (
    path TEXT PRIMARY KEY,
    checksum TEXT,
    size INTEGER,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS vectors (
    id TEXT PRIMARY KEY,
    source_id TEXT NOT NULL,
    embedding BLOB,
    metadata TEXT,
    model TEXT DEFAULT '',
    dim INTEGER DEFAULT 0,
    created_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS schema_migrations (
    version INTEGER PRIMARY KEY,
    applied_at TEXT NOT NULL
);
"""

_MIGRATIONS: list[tuple[int, str]] = [
    (1, "CREATE INDEX IF NOT EXISTS idx_sessions_updated ON sessions(updated_at)"),
    (2, "CREATE INDEX IF NOT EXISTS idx_logs_created ON logs(created_at)"),
    (3, "CREATE INDEX IF NOT EXISTS idx_vectors_source ON vectors(source_id)"),
    (
        4,
        """CREATE TABLE IF NOT EXISTS memory_episodes (
        id TEXT PRIMARY KEY,
        session_key TEXT NOT NULL,
        summary TEXT NOT NULL,
        message_range_start INTEGER NOT NULL DEFAULT 0,
        message_range_end INTEGER NOT NULL DEFAULT 0,
        entities TEXT DEFAULT '[]',
        importance REAL DEFAULT 0.5,
        created_at TEXT NOT NULL
    )""",
    ),
    (5, "CREATE INDEX IF NOT EXISTS idx_episodes_session ON memory_episodes(session_key)"),
    (
        6,
        """CREATE TABLE IF NOT EXISTS memory_graph_nodes (
        id TEXT PRIMARY KEY,
        label TEXT NOT NULL,
        node_type TEXT NOT NULL DEFAULT 'concept',
        properties TEXT DEFAULT '{}',
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL
    )""",
    ),
    (7, "CREATE INDEX IF NOT EXISTS idx_graph_nodes_label ON memory_graph_nodes(label COLLATE NOCASE)"),
    (
        8,
        """CREATE TABLE IF NOT EXISTS memory_graph_edges (
        id TEXT PRIMARY KEY,
        source_id TEXT NOT NULL,
        target_id TEXT NOT NULL,
        relation TEXT NOT NULL,
        weight REAL DEFAULT 1.0,
        valid_from TEXT,
        valid_to TEXT,
        source_memory_id TEXT,
        created_at TEXT NOT NULL
    )""",
    ),
    (9, "CREATE INDEX IF NOT EXISTS idx_graph_edges_source ON memory_graph_edges(source_id)"),
    (10, "CREATE INDEX IF NOT EXISTS idx_graph_edges_target ON memory_graph_edges(target_id)"),
    (
        11,
        """CREATE TABLE IF NOT EXISTS memory_contradictions (
        id TEXT PRIMARY KEY,
        memory_id_a TEXT NOT NULL,
        memory_id_b TEXT NOT NULL,
        description TEXT NOT NULL,
        resolution TEXT,
        resolved_at TEXT,
        created_at TEXT NOT NULL
    )""",
    ),
    (
        12,
        """CREATE TABLE IF NOT EXISTS memory_access_log (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        memory_id TEXT NOT NULL,
        accessed_at TEXT NOT NULL,
        context_query TEXT
    )""",
    ),
    (13, "CREATE INDEX IF NOT EXISTS idx_access_log_memory ON memory_access_log(memory_id)"),
    (14, "DROP TABLE IF EXISTS memory_graph_edges"),
    (15, "DROP TABLE IF EXISTS memory_graph_nodes"),
    (
        16,
        """CREATE TABLE IF NOT EXISTS message_archive (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        session_key TEXT NOT NULL,
        compression_id TEXT NOT NULL DEFAULT '',
        messages TEXT NOT NULL,
        message_count INTEGER NOT NULL DEFAULT 0,
        created_at TEXT NOT NULL
    )""",
    ),
    (17, "CREATE INDEX IF NOT EXISTS idx_archive_session ON message_archive(session_key)"),
    (
        18,
        """CREATE TABLE IF NOT EXISTS plan_runs (
        id TEXT PRIMARY KEY,
        session_key TEXT NOT NULL,
        trace_id TEXT NOT NULL DEFAULT '',
        goal TEXT NOT NULL DEFAULT '',
        strategy TEXT NOT NULL DEFAULT '',
        status TEXT NOT NULL DEFAULT 'running',
        current_step INTEGER NOT NULL DEFAULT 0,
        plan TEXT NOT NULL DEFAULT '{}',
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL
    )""",
    ),
    (19, "CREATE INDEX IF NOT EXISTS idx_plan_runs_session ON plan_runs(session_key)"),
    (
        20,
        """CREATE TABLE IF NOT EXISTS cost_ledger (
        window_date TEXT PRIMARY KEY,
        spent_usd REAL NOT NULL DEFAULT 0,
        input_tokens INTEGER NOT NULL DEFAULT 0,
        output_tokens INTEGER NOT NULL DEFAULT 0,
        cache_read_tokens INTEGER NOT NULL DEFAULT 0,
        cache_write_tokens INTEGER NOT NULL DEFAULT 0,
        updated_at TEXT NOT NULL DEFAULT ''
    )""",
    ),
    (
        21,
        """CREATE TABLE IF NOT EXISTS cost_ledger_dim (
        window_date TEXT NOT NULL DEFAULT '',
        provider TEXT NOT NULL DEFAULT '',
        model TEXT NOT NULL DEFAULT '',
        channel TEXT NOT NULL DEFAULT '',
        spent_usd REAL NOT NULL DEFAULT 0,
        input_tokens INTEGER NOT NULL DEFAULT 0,
        output_tokens INTEGER NOT NULL DEFAULT 0,
        cache_read_tokens INTEGER NOT NULL DEFAULT 0,
        cache_write_tokens INTEGER NOT NULL DEFAULT 0,
        updated_at TEXT NOT NULL DEFAULT '',
        PRIMARY KEY (window_date, provider, model, channel)
    )""",
    ),
    (22, "ALTER TABLE vectors ADD COLUMN model TEXT DEFAULT ''"),
    (23, "ALTER TABLE vectors ADD COLUMN dim INTEGER DEFAULT 0"),
    # 24 先清历史重复 episode(保留每组最早 id),否则 25 建唯一索引会失败。
    # (0,0) 区间是 legacy "无区间信息" 语义,用部分索引排除,保持逐次插入行为不变。
    (
        24,
        """DELETE FROM memory_episodes WHERE NOT (message_range_start = 0 AND message_range_end = 0)
            AND id NOT IN (SELECT MIN(id) FROM memory_episodes
            GROUP BY session_key, message_range_start, message_range_end)""",
    ),
    (
        25,
        """CREATE UNIQUE INDEX IF NOT EXISTS uq_episodes_span
            ON memory_episodes(session_key, message_range_start, message_range_end)
            WHERE NOT (message_range_start = 0 AND message_range_end = 0)""",
    ),
    # 26-29:tasks 表引入租约(owner/lease/attempt)+乐观锁 version,支撑
    # dispatcher 崩溃回收与终态 CAS。存量行:version=0、lease NULL、owner/attempt 空串,
    # 平滑升级。新库建表未含这些列,ALTER 报 duplicate column 由 _run_migrations 跳过。
    (26, "ALTER TABLE tasks ADD COLUMN owner_id TEXT NOT NULL DEFAULT ''"),
    (27, "ALTER TABLE tasks ADD COLUMN lease_until_ms INTEGER"),
    (28, "ALTER TABLE tasks ADD COLUMN attempt_id TEXT NOT NULL DEFAULT ''"),
    (29, "ALTER TABLE tasks ADD COLUMN version INTEGER NOT NULL DEFAULT 0"),
    (
        30,
        """CREATE TABLE IF NOT EXISTS turn_runs (
        event_id TEXT PRIMARY KEY,
        session_key TEXT NOT NULL,
        context_key TEXT NOT NULL DEFAULT '',
        trace_id TEXT NOT NULL DEFAULT '',
        status TEXT NOT NULL DEFAULT 'accepted',
        current_tool TEXT NOT NULL DEFAULT '',
        response_text TEXT NOT NULL DEFAULT '',
        error TEXT NOT NULL DEFAULT '',
        metadata TEXT NOT NULL DEFAULT '{}',
        created_at TEXT NOT NULL,
        started_at TEXT NOT NULL DEFAULT '',
        updated_at TEXT NOT NULL,
        completed_at TEXT NOT NULL DEFAULT ''
    )""",
    ),
    (31, "CREATE INDEX IF NOT EXISTS idx_turn_runs_session ON turn_runs(session_key, created_at DESC)"),
    (
        32,
        """CREATE TABLE IF NOT EXISTS inbound_idempotency (
        event_id TEXT PRIMARY KEY,
        namespace TEXT NOT NULL,
        fingerprint TEXT NOT NULL,
        session_key TEXT NOT NULL,
        status TEXT NOT NULL DEFAULT 'pending',
        response_text TEXT NOT NULL DEFAULT '',
        error TEXT NOT NULL DEFAULT '',
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL,
        expires_at REAL NOT NULL
    )""",
    ),
    (33, "CREATE INDEX IF NOT EXISTS idx_inbound_idempotency_expiry ON inbound_idempotency(expires_at)"),
    (
        34,
        """CREATE TABLE IF NOT EXISTS skill_usage_daily (
        window_date TEXT NOT NULL,
        skill TEXT NOT NULL,
        calls INTEGER NOT NULL DEFAULT 0,
        successes INTEGER NOT NULL DEFAULT 0,
        failures INTEGER NOT NULL DEFAULT 0,
        updated_at TEXT NOT NULL DEFAULT '',
        PRIMARY KEY (window_date, skill)
    )""",
    ),
]


class SQLiteBackend(StorageBackend):
    """Async SQLite storage with error recovery and auto-reconnect."""

    def __init__(self, db_path: Path):
        self._db_path = db_path
        self._db: aiosqlite.Connection | None = None
        self._connect_lock = asyncio.Lock()
        # aiosqlite serializes individual worker-thread calls, not a logical
        # execute+commit transaction.  Reads on the *same* connection can also
        # observe that connection's uncommitted writes.  One operation lock
        # therefore spans every public read/write and close, providing both a
        # real transaction boundary and a connection lifetime lease.
        self._operation_lock = asyncio.Lock()
        # Compatibility for embedders/tests that observed the former private
        # name. It is deliberately the same lock, not a second lock.
        self._write_lock = self._operation_lock

    @property
    def is_connected(self) -> bool:
        return self._db is not None

    async def initialize(self) -> None:
        self._db_path.parent.mkdir(parents=True, exist_ok=True)
        async with self._operation_lock:
            async with self._connect_lock:
                if self._db is None:
                    await self._connect()
        logger.info("SQLite storage initialized at {}", self._db_path)

    async def _connect(self) -> None:
        """Open and fully initialize a private connection before publishing it.

        Callers hold ``_connect_lock``.  Keeping ``self._db`` unset until schema
        setup commits prevents another operation from borrowing a half-migrated
        connection.
        """
        db = await aiosqlite.connect(str(self._db_path))
        try:
            await db.execute("PRAGMA journal_mode=WAL")
            await db.execute("PRAGMA foreign_keys=ON")
            await db.execute("PRAGMA busy_timeout=5000")
            await db.executescript(_SCHEMA_SQL)
            await self._run_migrations(db)
        except BaseException as connect_error:
            # A failed/cancelled migration must not leave a half-initialized
            # connection advertised as healthy, nor leak its worker thread.
            deferred_cancellation: asyncio.CancelledError | None = None
            try:
                deferred_cancellation = await _settle_cleanup(db.rollback())
            except BaseException as rollback_error:
                logger.debug("SQLite initialization rollback failed: {}", rollback_error)
            try:
                close_cancellation = await _settle_cleanup(db.close())
                deferred_cancellation = close_cancellation or deferred_cancellation
            except BaseException as close_error:
                logger.debug("SQLite initialization close failed: {}", close_error)
            if deferred_cancellation is not None and not isinstance(connect_error, asyncio.CancelledError):
                raise deferred_cancellation
            raise
        self._db = db

    async def _ensure_connection(self) -> aiosqlite.Connection:
        # Serialize the probe/reconnect path: without the lock, two tasks can
        # both see a dead connection and reconnect concurrently — leaking one
        # connection, or closing the one the other task just received.
        async with self._connect_lock:
            if self._db is None:
                await self._connect()
            try:
                await self._db.execute("SELECT 1")  # type: ignore[union-attr]
            except Exception:
                logger.warning("SQLite connection lost, reconnecting")
                old_db = self._db
                self._db = None
                if old_db is not None:
                    try:
                        deferred_cancellation = await _settle_cleanup(old_db.close())
                        if deferred_cancellation is not None:
                            raise deferred_cancellation
                    except Exception as e:
                        logger.debug("Failed to close stale SQLite connection: {}", e)
                await self._connect()
            return self._db  # type: ignore[return-value]

    async def _discard_connection(
        self,
        db: aiosqlite.Connection,
    ) -> asyncio.CancelledError | None:
        """Detach and settle-close a connection whose transaction is unsafe."""
        async with self._connect_lock:
            if self._db is db:
                self._db = None
            return await _settle_cleanup(db.close())

    async def _run_migrations(self, db: aiosqlite.Connection) -> None:
        # The connection is still private to _connect(), so no operation lock
        # is needed here.  It must not call _write_transaction(), which would
        # recursively acquire the operation lock during reconnect.
        rows = await db.execute_fetchall("SELECT version FROM schema_migrations")
        applied = {row[0] for row in rows}
        now = datetime.now().isoformat()
        try:
            for version, sql in _MIGRATIONS:
                if version in applied:
                    continue
                try:
                    await db.execute(sql)
                except Exception as exc:
                    # ALTER TABLE ADD COLUMN 在新建库（建表语句已含该列）上报
                    # duplicate column——等价于已应用，跳过而非中断启动。
                    if "duplicate column" in str(exc).lower():
                        logger.debug("Migration {} skipped (column exists)", version)
                    else:
                        raise
                await db.execute(
                    "INSERT INTO schema_migrations (version, applied_at) VALUES (?, ?)",
                    (version, now),
                )
            await db.commit()
        except BaseException as migration_error:
            try:
                deferred_cancellation = await _settle_cleanup(db.rollback())
            except BaseException as rollback_error:
                logger.warning("SQLite migration rollback failed: {}", rollback_error)
            else:
                if deferred_cancellation is not None and not isinstance(migration_error, asyncio.CancelledError):
                    raise deferred_cancellation
            raise

    @asynccontextmanager
    async def _read_connection(self):
        """Lease the live connection for one complete read operation."""
        async with self._operation_lock:
            yield await self._ensure_connection()

    @asynccontextmanager
    async def _write_transaction(self):
        """Lease the connection and guarantee rollback on error/cancellation."""
        async with self._operation_lock:
            db = await self._ensure_connection()
            try:
                yield db
                await db.commit()
            except BaseException as transaction_error:
                deferred_cancellation: asyncio.CancelledError | None = None
                try:
                    # Cancellation is itself one of the failure modes this path
                    # must repair. Keep the operation lease until rollback is
                    # truly terminal, even under repeated cancel() calls.
                    deferred_cancellation = await _settle_cleanup(db.rollback())
                except BaseException as rollback_error:
                    logger.warning("SQLite transaction rollback failed: {}", rollback_error)
                    # A connection with an unconfirmed rollback may still expose
                    # dirty rows to its own future reads. Remove it from service
                    # before releasing the operation lease; close itself rolls
                    # back SQLite transactions when possible.
                    try:
                        close_cancellation = await self._discard_connection(db)
                        deferred_cancellation = close_cancellation or deferred_cancellation
                    except BaseException as close_error:
                        logger.warning(
                            "Failed to discard unsafe SQLite connection: {}",
                            close_error,
                        )
                if deferred_cancellation is not None and not isinstance(transaction_error, asyncio.CancelledError):
                    raise deferred_cancellation
                raise

    async def close(self) -> None:
        # Wait for the current operation to finish before detaching/closing its
        # connection.  All normal operations take operation -> connect locks in
        # this order, so reconnect and close cannot deadlock each other.
        async with self._operation_lock:
            async with self._connect_lock:
                db, self._db = self._db, None
                if db:
                    try:
                        deferred_cancellation = await _settle_cleanup(db.close())
                    except Exception as e:
                        logger.debug("Failed to close SQLite connection on shutdown: {}", e)
                    else:
                        if deferred_cancellation is not None:
                            raise deferred_cancellation

    @staticmethod
    def _read_failure(operation: str, error: aiosqlite.Error | OSError) -> Exception:
        """Map physical read failures to the public storage error contract."""
        # SQLite's JSON1 extension reports corrupt stored JSON while evaluating
        # json_extract() as OperationalError.  The database is reachable; it is
        # the row that is unusable, so preserve the CorruptData distinction.
        if isinstance(error, aiosqlite.Error) and "malformed json" in str(error).lower():
            logger.error("Corrupt data while {}: {}", operation, error)
            return CorruptData(f"corrupt data while {operation}: {error}")
        logger.error("Storage unavailable while {}: {}", operation, error)
        return StorageUnavailable(f"storage unavailable while {operation}: {error}")

    @staticmethod
    def _decode_json(
        raw: Any,
        operation: str,
        *,
        expected_type: type | tuple[type, ...] | None = None,
    ) -> Any:
        """Decode one persisted JSON value or raise ``CorruptData``."""
        try:
            value = json.loads(raw)
        except (json.JSONDecodeError, ValueError, TypeError, KeyError) as e:
            logger.error("Corrupt JSON while {}: {}", operation, e)
            raise CorruptData(f"corrupt JSON while {operation}: {e}") from e
        if expected_type is not None and not isinstance(value, expected_type):
            expected = (
                ", ".join(item.__name__ for item in expected_type)
                if isinstance(expected_type, tuple)
                else expected_type.__name__
            )
            error = TypeError(f"expected {expected}, got {type(value).__name__}")
            logger.error("Corrupt JSON shape while {}: {}", operation, error)
            raise CorruptData(f"corrupt JSON shape while {operation}: {error}") from error
        return value

    # ── Session ────────────────────────────────────────────────────────────

    async def store_session(self, key: str, data: dict[str, Any]) -> None:
        now = datetime.now().isoformat()
        try:
            async with self._write_transaction() as db:
                await db.execute(
                    "INSERT OR REPLACE INTO sessions (key, data, created_at, updated_at) "
                    "VALUES (?, ?, COALESCE((SELECT created_at FROM sessions WHERE key=?), ?), ?)",
                    (key, json.dumps(data, ensure_ascii=False), key, now, now),
                )
        except Exception as e:
            logger.error("Failed to store session '{}': {}", key, e)
            raise

    async def load_session(self, key: str) -> dict[str, Any] | None:
        try:
            async with self._read_connection() as db:
                row = await db.execute_fetchall("SELECT data FROM sessions WHERE key=?", (key,))
        except (aiosqlite.Error, OSError) as e:
            raise self._read_failure(f"loading session '{key}'", e) from e
        if not row:
            return None
        return self._decode_json(
            row[0][0],
            f"loading session '{key}'",
            expected_type=dict,
        )

    async def delete_session(self, key: str) -> bool:
        try:
            async with self._write_transaction() as db:
                cursor = await db.execute("DELETE FROM sessions WHERE key=?", (key,))
            return cursor.rowcount > 0
        except Exception as e:
            logger.error("Failed to delete session '{}': {}", key, e)
            return False

    async def list_sessions(self) -> list[dict[str, Any]]:
        try:
            async with self._read_connection() as db:
                rows = await db.execute_fetchall(
                    "SELECT key, data, created_at, updated_at FROM sessions ORDER BY updated_at DESC"
                )
        except (aiosqlite.Error, OSError) as e:
            raise self._read_failure("listing sessions", e) from e

        sessions: list[dict[str, Any]] = []
        for key, raw, created_at, updated_at in rows:
            data = self._decode_json(
                raw,
                f"listing session '{key}'",
                expected_type=dict,
            )
            messages = data.get("messages", [])
            metadata = data.get("metadata", {})
            if not isinstance(messages, list) or not isinstance(metadata, dict):
                error = TypeError("session messages must be a list and metadata must be an object")
                raise CorruptData(f"corrupt JSON shape while listing session '{key}': {error}")
            sessions.append(
                {
                    "key": key,
                    "status": data.get("status", "active"),
                    "created_at": data.get("created_at") or created_at,
                    "updated_at": data.get("updated_at") or updated_at,
                    "metadata": metadata,
                    "message_count": len(messages),
                }
            )
        return sessions

    # ── Memory ─────────────────────────────────────────────────────────────

    async def store_memory(self, entry_id: str, data: dict[str, Any]) -> None:
        now = datetime.now().isoformat()
        try:
            async with self._write_transaction() as db:
                await db.execute(
                    "INSERT OR REPLACE INTO memories (id, type, key, data, created_at, updated_at) "
                    "VALUES (?, ?, ?, ?, COALESCE((SELECT created_at FROM memories WHERE id=?), ?), ?)",
                    (
                        entry_id,
                        data.get("type", "user"),
                        data.get("key", ""),
                        json.dumps(data, ensure_ascii=False),
                        entry_id,
                        now,
                        now,
                    ),
                )
        except Exception as e:
            logger.error("Failed to store memory '{}': {}", entry_id, e)
            raise

    async def load_memories(self, mem_type: str | None = None) -> list[dict[str, Any]]:
        try:
            async with self._read_connection() as db:
                if mem_type:
                    rows = await db.execute_fetchall(
                        "SELECT data FROM memories WHERE type=? ORDER BY updated_at DESC",
                        (mem_type,),
                    )
                else:
                    rows = await db.execute_fetchall("SELECT data FROM memories ORDER BY updated_at DESC")
        except (aiosqlite.Error, OSError) as e:
            raise self._read_failure("loading memories", e) from e
        return [self._decode_json(raw, "loading a memory row", expected_type=dict) for (raw,) in rows]

    async def delete_memory(self, entry_id: str) -> bool:
        try:
            async with self._write_transaction() as db:
                cursor = await db.execute("DELETE FROM memories WHERE id=?", (entry_id,))
            return cursor.rowcount > 0
        except Exception as e:
            logger.error("Failed to delete memory '{}': {}", entry_id, e)
            return False

    # ── Task ───────────────────────────────────────────────────────────────

    async def store_task(self, task_id: str, data: dict[str, Any]) -> None:
        now = datetime.now().isoformat()
        try:
            async with self._write_transaction() as db:
                await db.execute(
                    "INSERT OR REPLACE INTO tasks "
                    "(id, workflow_id, status, data, owner_id, lease_until_ms, attempt_id, version, created_at, updated_at) "
                    "VALUES (?, ?, ?, ?, ?, ?, ?, ?, COALESCE((SELECT created_at FROM tasks WHERE id=?), ?), ?)",
                    (
                        task_id,
                        data.get("workflow_id", ""),
                        data.get("status", "pending"),
                        json.dumps(data, ensure_ascii=False),
                        data.get("owner_id", ""),
                        data.get("lease_until_ms"),
                        data.get("attempt_id", ""),
                        data.get("version", 0),
                        task_id,
                        now,
                        now,
                    ),
                )
        except Exception as e:
            logger.error("Failed to store task '{}': {}", task_id, e)
            raise

    async def cas_store_task(self, task_id: str, data: dict[str, Any], expected_version: int) -> bool:
        """Compare-and-swap write: only persists when the row's current version
        still equals ``expected_version``, then bumps it. Returns True on a
        winning swap, False when another writer already advanced the version
        (stale caller must re-read). This is the one write path allowed to move a
        task's terminal/lease state under concurrency without clobbering a peer."""
        now = datetime.now().isoformat()
        # Make the stored JSON authoritative: on a winning swap the SQL bumps the
        # ``version`` column to expected_version+1, so the serialized blob must
        # carry the same value regardless of what the caller passed. Otherwise the
        # column and load_task(...)["version"] diverge and a read-modify-write
        # retry loop spuriously loses. Shallow-copy to avoid mutating the caller.
        persisted = {**data, "version": expected_version + 1}
        try:
            async with self._write_transaction() as db:
                cur = await db.execute(
                    "UPDATE tasks SET status=?, data=?, owner_id=?, lease_until_ms=?, "
                    "attempt_id=?, version=version+1, updated_at=? WHERE id=? AND version=?",
                    (
                        persisted.get("status", "pending"),
                        json.dumps(persisted, ensure_ascii=False),
                        persisted.get("owner_id", ""),
                        persisted.get("lease_until_ms"),
                        persisted.get("attempt_id", ""),
                        now,
                        task_id,
                        expected_version,
                    ),
                )
            return cur.rowcount == 1
        except Exception as e:
            logger.error("CAS store task '{}' failed: {}", task_id, e)
            raise

    async def load_task(self, task_id: str) -> dict[str, Any] | None:
        try:
            async with self._read_connection() as db:
                row = await db.execute_fetchall("SELECT data FROM tasks WHERE id=?", (task_id,))
        except (aiosqlite.Error, OSError) as e:
            raise self._read_failure(f"loading task '{task_id}'", e) from e
        if not row:
            return None
        return self._decode_json(
            row[0][0],
            f"loading task '{task_id}'",
            expected_type=dict,
        )

    async def list_tasks(
        self,
        workflow_id: str | None = None,
        status: str | None = None,
        board_id: str | None = None,
        assignee: str | None = None,
        label: str | None = None,
    ) -> list[dict[str, Any]]:
        clauses: list[str] = []
        params: list[str] = []
        if workflow_id:
            clauses.append("workflow_id=?")
            params.append(workflow_id)
        if status:
            clauses.append("status=?")
            params.append(status)
        if board_id:
            clauses.append("json_extract(data, '$.board_id')=?")
            params.append(board_id)
        if assignee:
            clauses.append("json_extract(data, '$.assignee')=?")
            params.append(assignee)
        where = " WHERE " + " AND ".join(clauses) if clauses else ""
        try:
            async with self._read_connection() as db:
                rows = await db.execute_fetchall(
                    f"SELECT data FROM tasks{where} ORDER BY updated_at DESC",
                    params,
                )
        except (aiosqlite.Error, OSError) as e:
            raise self._read_failure("listing tasks", e) from e

        results = [self._decode_json(raw, "listing a task row", expected_type=dict) for (raw,) in rows]
        for result in results:
            labels = result.get("labels", [])
            if not isinstance(labels, list):
                error = TypeError("task labels must be a list")
                raise CorruptData(f"corrupt JSON shape while listing tasks: {error}")
        if label:
            results = [result for result in results if label in result.get("labels", [])]
        return results

    # ── Workflow ────────────────────────────────────────────────────────────

    async def store_workflow(self, workflow_id: str, data: dict[str, Any]) -> None:
        now = datetime.now().isoformat()
        try:
            async with self._write_transaction() as db:
                await db.execute(
                    "INSERT OR REPLACE INTO workflows (id, name, status, data, created_at, updated_at) "
                    "VALUES (?, ?, ?, ?, COALESCE((SELECT created_at FROM workflows WHERE id=?), ?), ?)",
                    (
                        workflow_id,
                        data.get("name", ""),
                        data.get("status", "pending"),
                        json.dumps(data, ensure_ascii=False),
                        workflow_id,
                        now,
                        now,
                    ),
                )
        except Exception as e:
            logger.error("Failed to store workflow '{}': {}", workflow_id, e)
            raise

    async def load_workflow(self, workflow_id: str) -> dict[str, Any] | None:
        try:
            async with self._read_connection() as db:
                row = await db.execute_fetchall(
                    "SELECT data FROM workflows WHERE id=?",
                    (workflow_id,),
                )
        except (aiosqlite.Error, OSError) as e:
            raise self._read_failure(f"loading workflow '{workflow_id}'", e) from e
        if not row:
            return None
        return self._decode_json(
            row[0][0],
            f"loading workflow '{workflow_id}'",
            expected_type=dict,
        )

    async def list_workflows(self, status: str | None = None) -> list[dict[str, Any]]:
        try:
            async with self._read_connection() as db:
                if status:
                    rows = await db.execute_fetchall(
                        "SELECT data FROM workflows WHERE status=? ORDER BY updated_at DESC",
                        (status,),
                    )
                else:
                    rows = await db.execute_fetchall("SELECT data FROM workflows ORDER BY updated_at DESC")
        except (aiosqlite.Error, OSError) as e:
            raise self._read_failure("listing workflows", e) from e
        return [self._decode_json(raw, "listing a workflow row", expected_type=dict) for (raw,) in rows]

    # ── Log ────────────────────────────────────────────────────────────────

    async def store_log(self, trace_id: str, spans: list[dict[str, Any]]) -> None:
        now = datetime.now().isoformat()
        try:
            async with self._write_transaction() as db:
                await db.execute(
                    "INSERT INTO logs (trace_id, data, created_at) VALUES (?, ?, ?)",
                    (trace_id, json.dumps(spans, ensure_ascii=False), now),
                )
        except Exception as e:
            logger.error("Failed to store log for trace '{}': {}", trace_id, e)

    async def query_logs(self, filters: dict[str, Any] | None = None, limit: int = 100) -> list[dict[str, Any]]:
        try:
            async with self._read_connection() as db:
                rows = await db.execute_fetchall(
                    "SELECT trace_id, data, created_at FROM logs ORDER BY id DESC LIMIT ?",
                    (limit,),
                )
        except (aiosqlite.Error, OSError) as e:
            raise self._read_failure("querying logs", e) from e
        return [
            {
                "trace_id": trace_id,
                "spans": self._decode_json(
                    raw,
                    f"querying log '{trace_id}'",
                    expected_type=list,
                ),
                "created_at": created_at,
            }
            for trace_id, raw, created_at in rows
        ]

    # ── File & Vector ──────────────────────────────────────────────────────

    async def store_file_meta(self, path: str, checksum: str, size: int) -> None:
        try:
            async with self._write_transaction() as db:
                await db.execute(
                    "INSERT OR REPLACE INTO files (path, checksum, size, updated_at) VALUES (?, ?, ?, ?)",
                    (path, checksum, size, datetime.now().isoformat()),
                )
        except Exception as e:
            logger.error("Failed to store file meta '{}': {}", path, e)

    async def store_vector(
        self,
        vec_id: str,
        source_id: str,
        embedding: bytes,
        metadata: dict[str, Any] | None = None,
        model: str = "",
        dim: int = 0,
    ) -> None:
        try:
            async with self._write_transaction() as db:
                await db.execute(
                    "INSERT OR REPLACE INTO vectors (id, source_id, embedding, metadata, model, dim, created_at) "
                    "VALUES (?, ?, ?, ?, ?, ?, ?)",
                    (vec_id, source_id, embedding, json.dumps(metadata or {}), model, dim, datetime.now().isoformat()),
                )
        except Exception as e:
            logger.error("Failed to store vector '{}': {}", vec_id, e)

    async def load_vectors_all(self) -> list[dict[str, Any]]:
        try:
            async with self._read_connection() as db:
                rows = await db.execute_fetchall("SELECT id, source_id, embedding, metadata, model, dim FROM vectors")
        except (aiosqlite.Error, OSError) as e:
            raise self._read_failure("loading vectors", e) from e
        return [self._decode_vector_row(row) for row in rows]

    async def load_vector_by_source(self, source_id: str) -> dict[str, Any] | None:
        try:
            async with self._read_connection() as db:
                rows = await db.execute_fetchall(
                    "SELECT id, source_id, embedding, metadata, model, dim FROM vectors WHERE source_id=?",
                    (source_id,),
                )
        except (aiosqlite.Error, OSError) as e:
            raise self._read_failure(f"loading vector source '{source_id}'", e) from e
        return self._decode_vector_row(rows[0]) if rows else None

    def _decode_vector_row(self, row: tuple[Any, ...]) -> dict[str, Any]:
        vector_id, source_id, embedding, raw_metadata, model, dim = row
        return {
            "id": vector_id,
            "source_id": source_id,
            "embedding": embedding,
            "metadata": self._decode_json(
                raw_metadata,
                f"loading vector '{vector_id}' metadata",
                expected_type=dict,
            ),
            "model": model or "",
            "dim": dim or 0,
        }

    async def delete_vector(self, vec_id: str) -> None:
        try:
            async with self._write_transaction() as db:
                await db.execute("DELETE FROM vectors WHERE id=?", (vec_id,))
        except Exception as e:
            logger.error("Failed to delete vector '{}': {}", vec_id, e)

    async def execute_sql(self, sql: str, params: tuple = ()) -> int:
        try:
            async with self._write_transaction() as db:
                cursor = await db.execute(sql, params)
            return cursor.rowcount
        except Exception as e:
            logger.error("SQL execute failed: {}", e)
            raise

    async def fetch_sql(self, sql: str, params: tuple = ()) -> list[dict[str, Any]]:
        try:
            async with self._read_connection() as db:
                cursor = await db.execute(sql, params)
                columns = [desc[0] for desc in cursor.description] if cursor.description else []
                rows = await cursor.fetchall()
        except (aiosqlite.Error, OSError) as e:
            raise self._read_failure("fetching SQL rows", e) from e
        return [dict(zip(columns, row)) for row in rows]

    # ── Message Archive ────────────────────────────────────────────────────

    async def archive_messages(
        self, session_key: str, messages: list[dict[str, Any]], compression_id: str = ""
    ) -> None:
        now = datetime.now().isoformat()
        try:
            async with self._write_transaction() as db:
                await db.execute(
                    "INSERT INTO message_archive (session_key, compression_id, messages, message_count, created_at) "
                    "VALUES (?, ?, ?, ?, ?)",
                    (session_key, compression_id, json.dumps(messages, ensure_ascii=False), len(messages), now),
                )
        except Exception as e:
            logger.error("Failed to archive messages for session '{}': {}", session_key, e)

    async def load_archived_messages(self, session_key: str, limit: int = 100) -> list[dict[str, Any]]:
        try:
            async with self._read_connection() as db:
                cursor = await db.execute(
                    "SELECT messages, compression_id, created_at FROM message_archive "
                    "WHERE session_key = ? ORDER BY created_at DESC LIMIT ?",
                    (session_key, limit),
                )
                rows = await cursor.fetchall()
        except (aiosqlite.Error, OSError) as e:
            raise self._read_failure(
                f"loading archived messages for '{session_key}'",
                e,
            ) from e
        return [
            {
                "messages": self._decode_json(
                    raw_messages,
                    f"loading archived messages for '{session_key}'",
                    expected_type=list,
                ),
                "compression_id": compression_id,
                "created_at": created_at,
            }
            for raw_messages, compression_id, created_at in rows
        ]
