"""Concurrency, rollback, migration, and long-tail API tests for SQLite."""

from __future__ import annotations

import asyncio
from pathlib import Path

import aiosqlite
import pytest
import pytest_asyncio

from echo_agent.storage.sqlite import SQLiteBackend


@pytest_asyncio.fixture
async def backend(tmp_path: Path):
    storage = SQLiteBackend(tmp_path / "resilience.db")
    await storage.initialize()
    yield storage
    await storage.close()


@pytest.mark.asyncio
async def test_failed_commit_rolls_back_partial_write(
    backend: SQLiteBackend,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    db = await backend._ensure_connection()
    original_commit = db.commit

    async def fail_commit() -> None:
        raise aiosqlite.OperationalError("simulated disk full")

    monkeypatch.setattr(db, "commit", fail_commit)
    with pytest.raises(aiosqlite.OperationalError, match="disk full"):
        await backend.store_session("partial", {"messages": []})

    # Inspect on the same connection: without an explicit rollback, SQLite
    # would still expose the uncommitted row here and a later unrelated commit
    # could make it durable.
    rows = await db.execute_fetchall("SELECT key FROM sessions WHERE key='partial'")
    assert rows == []

    monkeypatch.setattr(db, "commit", original_commit)
    await backend.store_session("after-failure", {"messages": []})
    assert await backend.load_session("after-failure") is not None


@pytest.mark.asyncio
async def test_cancelled_write_rolls_back_before_propagating(
    backend: SQLiteBackend,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    db = await backend._ensure_connection()
    original_execute = db.execute

    async def cancel_after_execute(sql: str, parameters=None):
        cursor = await original_execute(sql, parameters)
        if sql.startswith("INSERT OR REPLACE INTO sessions"):
            raise asyncio.CancelledError
        return cursor

    monkeypatch.setattr(db, "execute", cancel_after_execute)
    with pytest.raises(asyncio.CancelledError):
        await backend.store_session("cancelled", {"messages": []})

    rows = await db.execute_fetchall("SELECT key FROM sessions WHERE key='cancelled'")
    assert rows == []


@pytest.mark.asyncio
async def test_concurrent_read_waits_for_failed_write_rollback(
    backend: SQLiteBackend,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    db = await backend._ensure_connection()
    original_execute = db.execute
    write_executed = asyncio.Event()
    release_failure = asyncio.Event()

    async def fail_after_uncommitted_execute(sql: str, parameters=None):
        cursor = await original_execute(sql, parameters)
        if sql.startswith("INSERT OR REPLACE INTO sessions"):
            write_executed.set()
            await release_failure.wait()
            raise aiosqlite.OperationalError("forced rollback")
        return cursor

    monkeypatch.setattr(db, "execute", fail_after_uncommitted_execute)
    write_task = asyncio.create_task(
        backend.store_session("never-visible", {"messages": []})
    )
    await write_executed.wait()
    read_task = asyncio.create_task(backend.load_session("never-visible"))
    await asyncio.sleep(0)

    # A read on the same SQLite connection would otherwise see the writer's
    # uncommitted row. It must wait for the transaction outcome instead.
    assert not read_task.done()
    release_failure.set()
    with pytest.raises(aiosqlite.OperationalError, match="forced rollback"):
        await write_task
    assert await read_task is None


@pytest.mark.asyncio
async def test_concurrent_read_waits_for_cancelled_write_rollback(
    backend: SQLiteBackend,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    db = await backend._ensure_connection()
    original_execute = db.execute
    write_executed = asyncio.Event()
    keep_uncommitted = asyncio.Event()

    async def block_after_uncommitted_execute(sql: str, parameters=None):
        cursor = await original_execute(sql, parameters)
        if sql.startswith("INSERT OR REPLACE INTO sessions"):
            write_executed.set()
            await keep_uncommitted.wait()
        return cursor

    monkeypatch.setattr(db, "execute", block_after_uncommitted_execute)
    write_task = asyncio.create_task(
        backend.store_session("cancelled-before-read", {"messages": []})
    )
    await write_executed.wait()
    read_task = asyncio.create_task(backend.load_session("cancelled-before-read"))
    await asyncio.sleep(0)
    assert not read_task.done()

    write_task.cancel()
    with pytest.raises(asyncio.CancelledError):
        await write_task
    assert await read_task is None


@pytest.mark.asyncio
async def test_repeated_cancellation_cannot_release_slow_rollback_lease(
    backend: SQLiteBackend,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    db = await backend._ensure_connection()
    original_execute = db.execute
    original_rollback = db.rollback
    write_executed = asyncio.Event()
    rollback_started = asyncio.Event()
    release_rollback = asyncio.Event()

    async def leave_uncommitted_then_wait(sql: str, parameters=None):
        cursor = await original_execute(sql, parameters)
        if sql.startswith("INSERT OR REPLACE INTO sessions"):
            write_executed.set()
            await asyncio.Future()
        return cursor

    async def slow_rollback() -> None:
        rollback_started.set()
        await release_rollback.wait()
        await original_rollback()

    monkeypatch.setattr(db, "execute", leave_uncommitted_then_wait)
    monkeypatch.setattr(db, "rollback", slow_rollback)
    write_task = asyncio.create_task(
        backend.store_session("never-dirty", {"messages": []})
    )
    await write_executed.wait()
    write_task.cancel()
    await rollback_started.wait()

    read_task = asyncio.create_task(backend.load_session("never-dirty"))
    await asyncio.sleep(0)
    close_task = asyncio.create_task(backend.close())
    await asyncio.sleep(0)
    assert read_task.done() is False
    assert close_task.done() is False

    # The second cancellation used to break out of asyncio.shield(), release
    # _operation_lock, and let this read observe the uncommitted row while the
    # rollback task continued in the background.
    write_task.cancel()
    await asyncio.sleep(0)
    assert write_task.done() is False
    assert read_task.done() is False
    assert close_task.done() is False

    release_rollback.set()
    with pytest.raises(asyncio.CancelledError):
        await write_task
    assert await read_task is None
    await asyncio.wait_for(close_task, timeout=1.0)
    assert backend._db is None


@pytest.mark.asyncio
async def test_failed_rollback_discards_connection_before_next_read(
    backend: SQLiteBackend,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    db = await backend._ensure_connection()
    original_execute = db.execute

    async def write_then_fail(sql: str, parameters=None):
        cursor = await original_execute(sql, parameters)
        if sql.startswith("INSERT OR REPLACE INTO sessions"):
            raise aiosqlite.OperationalError("write failed after execute")
        return cursor

    async def rollback_fails() -> None:
        raise aiosqlite.OperationalError("rollback failed")

    monkeypatch.setattr(db, "execute", write_then_fail)
    monkeypatch.setattr(db, "rollback", rollback_fails)

    with pytest.raises(aiosqlite.OperationalError, match="write failed"):
        await backend.store_session("unsafe", {"messages": []})
    assert backend._db is None

    # The next read reconnects instead of borrowing the connection whose
    # rollback outcome was unknown. SQLite close resolves its transaction.
    assert await backend.load_session("unsafe") is None
    assert backend._db is not db


@pytest.mark.asyncio
async def test_close_waits_for_active_read_connection_lease(
    backend: SQLiteBackend,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    await backend.store_session("leased", {"messages": []})
    db = await backend._ensure_connection()
    original_execute_fetchall = db.execute_fetchall
    read_started = asyncio.Event()
    release_read = asyncio.Event()

    async def blocked_read(sql: str, parameters=None):
        if sql.startswith("SELECT data FROM sessions"):
            read_started.set()
            await release_read.wait()
        return await original_execute_fetchall(sql, parameters)

    monkeypatch.setattr(db, "execute_fetchall", blocked_read)
    read_task = asyncio.create_task(backend.load_session("leased"))
    await read_started.wait()
    close_task = asyncio.create_task(backend.close())
    await asyncio.sleep(0)

    assert not close_task.done()
    assert backend._db is db
    release_read.set()
    assert await read_task == {"messages": []}
    await asyncio.wait_for(close_task, timeout=1.0)
    assert backend._db is None


@pytest.mark.asyncio
async def test_database_lock_failure_rolls_back_and_connection_recovers(
    backend: SQLiteBackend,
) -> None:
    db = await backend._ensure_connection()
    await db.execute("PRAGMA busy_timeout=20")
    await db.commit()

    blocker = await aiosqlite.connect(str(backend._db_path))
    try:
        await blocker.execute("BEGIN IMMEDIATE")
        with pytest.raises(aiosqlite.OperationalError, match="locked"):
            await backend.store_session("locked-write", {"messages": []})
        await blocker.rollback()

        await backend.store_session("recovered-write", {"messages": []})
        assert await backend.load_session("recovered-write") is not None
    finally:
        await blocker.close()


@pytest.mark.asyncio
async def test_failed_migration_does_not_advertise_or_leak_connection(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    from echo_agent.storage import sqlite as sqlite_module

    original_migrations = sqlite_module._MIGRATIONS
    monkeypatch.setattr(
        sqlite_module,
        "_MIGRATIONS",
        [(9999, "THIS IS DELIBERATELY INVALID SQL")],
    )
    storage = SQLiteBackend(tmp_path / "migration.db")

    with pytest.raises(aiosqlite.Error):
        await storage.initialize()
    assert storage.is_connected is False
    assert storage._db is None

    # The failed migration version was rolled back, so a corrected process can
    # initialize the exact same file rather than inheriting a false success.
    monkeypatch.setattr(sqlite_module, "_MIGRATIONS", original_migrations)
    await storage.initialize()
    versions = await storage.fetch_sql(
        "SELECT version FROM schema_migrations WHERE version=9999"
    )
    assert versions == []
    await storage.close()


@pytest.mark.asyncio
async def test_closed_but_non_null_connection_is_replaced(tmp_path: Path) -> None:
    storage = SQLiteBackend(tmp_path / "dead-connection.db")
    await storage.initialize()
    await storage.store_session("survives", {"messages": []})
    stale = storage._db
    assert stale is not None
    await stale.close()

    assert await storage.load_session("survives") is not None
    assert storage._db is not stale
    await storage.close()


@pytest.mark.asyncio
async def test_workflow_log_file_vector_raw_sql_and_archive_round_trip(
    backend: SQLiteBackend,
) -> None:
    await backend.store_workflow("w-pending", {"name": "one", "status": "pending"})
    await backend.store_workflow("w-done", {"name": "two", "status": "done"})
    assert (await backend.load_workflow("w-pending"))["name"] == "one"  # type: ignore[index]
    assert [item["name"] for item in await backend.list_workflows("done")] == ["two"]
    assert len(await backend.list_workflows()) == 2

    await backend.store_log("trace-1", [{"name": "step"}])
    logs = await backend.query_logs(limit=1)
    assert logs[0]["trace_id"] == "trace-1"
    assert logs[0]["spans"] == [{"name": "step"}]

    await backend.store_file_meta("artifact.txt", "sha256", 42)
    assert await backend.fetch_sql(
        "SELECT checksum, size FROM files WHERE path=?", ("artifact.txt",)
    ) == [{"checksum": "sha256", "size": 42}]

    await backend.store_vector(
        "vec-1", "source-1", b"\x01\x02", {"kind": "test"}, model="embed-a", dim=2
    )
    vector = await backend.load_vector_by_source("source-1")
    assert vector is not None
    assert vector["id"] == "vec-1"
    assert vector["model"] == "embed-a"
    assert vector["dim"] == 2
    assert len(await backend.load_vectors_all()) == 1
    await backend.delete_vector("vec-1")
    assert await backend.load_vector_by_source("source-1") is None

    await backend.execute_sql("CREATE TABLE audit_probe (value TEXT)")
    await backend.execute_sql("INSERT INTO audit_probe (value) VALUES (?)", ("ok",))
    assert await backend.fetch_sql("SELECT value FROM audit_probe") == [{"value": "ok"}]

    messages = [{"role": "user", "content": "remember"}]
    await backend.archive_messages("session-1", messages, "compression-1")
    archived = await backend.load_archived_messages("session-1")
    assert archived[0]["messages"] == messages
    assert archived[0]["compression_id"] == "compression-1"


@pytest.mark.asyncio
async def test_task_cas_and_json_filters(backend: SQLiteBackend) -> None:
    await backend.store_task(
        "task-1",
        {
            "status": "pending",
            "workflow_id": "workflow-1",
            "board_id": "board-1",
            "assignee": "alice",
            "labels": ["urgent", "backend"],
            "version": 0,
        },
    )
    assert len(await backend.list_tasks(board_id="board-1")) == 1
    assert len(await backend.list_tasks(assignee="alice")) == 1
    assert len(await backend.list_tasks(label="urgent")) == 1
    assert await backend.list_tasks(label="frontend") == []

    task = await backend.load_task("task-1")
    assert task is not None
    assert await backend.cas_store_task("task-1", {**task, "status": "running"}, 0) is True
    assert await backend.cas_store_task("task-1", {**task, "status": "done"}, 0) is False
    updated = await backend.load_task("task-1")
    assert updated is not None
    assert updated["status"] == "running"
    assert updated["version"] == 1
