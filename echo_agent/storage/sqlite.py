"""SQLite storage backend implementation."""

from __future__ import annotations

import json
import sqlite3
from datetime import datetime
from pathlib import Path
from typing import Any

from loguru import logger

from echo_agent.storage.backend import StorageBackend


class SQLiteBackend(StorageBackend):
    """SQLite-based storage for all persistent data."""

    def __init__(self, db_path: Path):
        self._db_path = db_path
        self._conn: sqlite3.Connection | None = None

    async def initialize(self) -> None:
        self._db_path.parent.mkdir(parents=True, exist_ok=True)
        self._conn = sqlite3.connect(str(self._db_path), check_same_thread=False)
        self._conn.execute("PRAGMA journal_mode=WAL")
        self._conn.execute("PRAGMA foreign_keys=ON")
        self._create_tables()
        logger.info("SQLite storage initialized at {}", self._db_path)

    def _create_tables(self) -> None:
        assert self._conn
        self._conn.executescript("""
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
                created_at TEXT NOT NULL
            );
        """)
        self._conn.commit()

    async def close(self) -> None:
        if self._conn:
            self._conn.close()
            self._conn = None

    async def store_session(self, key: str, data: dict[str, Any]) -> None:
        assert self._conn
        now = datetime.now().isoformat()
        self._conn.execute(
            "INSERT OR REPLACE INTO sessions (key, data, created_at, updated_at) VALUES (?, ?, COALESCE((SELECT created_at FROM sessions WHERE key=?), ?), ?)",
            (key, json.dumps(data, ensure_ascii=False), key, now, now),
        )
        self._conn.commit()

    async def load_session(self, key: str) -> dict[str, Any] | None:
        assert self._conn
        row = self._conn.execute("SELECT data FROM sessions WHERE key=?", (key,)).fetchone()
        return json.loads(row[0]) if row else None

    async def store_memory(self, entry_id: str, data: dict[str, Any]) -> None:
        assert self._conn
        now = datetime.now().isoformat()
        self._conn.execute(
            "INSERT OR REPLACE INTO memories (id, type, key, data, created_at, updated_at) VALUES (?, ?, ?, ?, COALESCE((SELECT created_at FROM memories WHERE id=?), ?), ?)",
            (entry_id, data.get("type", "user"), data.get("key", ""), json.dumps(data, ensure_ascii=False), entry_id, now, now),
        )
        self._conn.commit()

    async def load_memories(self, mem_type: str | None = None) -> list[dict[str, Any]]:
        assert self._conn
        if mem_type:
            rows = self._conn.execute("SELECT data FROM memories WHERE type=? ORDER BY updated_at DESC", (mem_type,)).fetchall()
        else:
            rows = self._conn.execute("SELECT data FROM memories ORDER BY updated_at DESC").fetchall()
        return [json.loads(r[0]) for r in rows]

    async def store_task(self, task_id: str, data: dict[str, Any]) -> None:
        assert self._conn
        now = datetime.now().isoformat()
        self._conn.execute(
            "INSERT OR REPLACE INTO tasks (id, workflow_id, status, data, created_at, updated_at) VALUES (?, ?, ?, ?, COALESCE((SELECT created_at FROM tasks WHERE id=?), ?), ?)",
            (task_id, data.get("workflow_id", ""), data.get("status", "pending"), json.dumps(data, ensure_ascii=False), task_id, now, now),
        )
        self._conn.commit()

    async def load_task(self, task_id: str) -> dict[str, Any] | None:
        assert self._conn
        row = self._conn.execute("SELECT data FROM tasks WHERE id=?", (task_id,)).fetchone()
        return json.loads(row[0]) if row else None

    async def store_log(self, trace_id: str, spans: list[dict[str, Any]]) -> None:
        assert self._conn
        now = datetime.now().isoformat()
        self._conn.execute(
            "INSERT INTO logs (trace_id, data, created_at) VALUES (?, ?, ?)",
            (trace_id, json.dumps(spans, ensure_ascii=False), now),
        )
        self._conn.commit()

    async def query_logs(self, filters: dict[str, Any] | None = None, limit: int = 100) -> list[dict[str, Any]]:
        assert self._conn
        query = "SELECT trace_id, data, created_at FROM logs ORDER BY id DESC LIMIT ?"
        rows = self._conn.execute(query, (limit,)).fetchall()
        return [{"trace_id": r[0], "spans": json.loads(r[1]), "created_at": r[2]} for r in rows]

    async def store_file_meta(self, path: str, checksum: str, size: int) -> None:
        assert self._conn
        self._conn.execute(
            "INSERT OR REPLACE INTO files (path, checksum, size, updated_at) VALUES (?, ?, ?, ?)",
            (path, checksum, size, datetime.now().isoformat()),
        )
        self._conn.commit()

    async def store_vector(self, vec_id: str, source_id: str, embedding: bytes, metadata: dict[str, Any] | None = None) -> None:
        assert self._conn
        self._conn.execute(
            "INSERT OR REPLACE INTO vectors (id, source_id, embedding, metadata, created_at) VALUES (?, ?, ?, ?, ?)",
            (vec_id, source_id, embedding, json.dumps(metadata or {}), datetime.now().isoformat()),
        )
        self._conn.commit()
