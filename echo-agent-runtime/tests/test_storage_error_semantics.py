from pathlib import Path

import aiosqlite
import pytest
import pytest_asyncio

from echo_agent.storage.sqlite import SQLiteBackend
from echo_agent.storage.errors import StorageError, StorageUnavailable, CorruptData


def test_exception_hierarchy():
    assert issubclass(StorageUnavailable, StorageError)
    assert issubclass(CorruptData, StorageError)
    assert issubclass(StorageError, Exception)


def test_exceptions_carry_message():
    err = StorageUnavailable("db is gone")
    assert str(err) == "db is gone"
    assert isinstance(err, StorageError)


@pytest_asyncio.fixture
async def backend(tmp_path: Path):
    db = SQLiteBackend(tmp_path / "sem.db")
    await db.initialize()
    yield db
    await db.close()


@pytest.mark.asyncio
async def test_load_session_missing_returns_none(backend: SQLiteBackend):
    assert await backend.load_session("nope") is None


@pytest.mark.asyncio
async def test_load_session_corrupt_row_raises_corrupt_data(backend: SQLiteBackend):
    # Write a row whose data column is not valid JSON, bypassing store_session.
    db = await backend._ensure_connection()
    await db.execute(
        "INSERT OR REPLACE INTO sessions (key, data, created_at, updated_at) "
        "VALUES (?, ?, ?, ?)",
        ("bad:1", "{not json", "2026-01-01T00:00:00", "2026-01-01T00:00:00"),
    )
    await db.commit()
    with pytest.raises(CorruptData):
        await backend.load_session("bad:1")


@pytest.mark.asyncio
async def test_load_session_io_error_raises_unavailable(backend: SQLiteBackend, monkeypatch):
    async def boom(*a, **k):
        raise aiosqlite.OperationalError("disk I/O error")

    db = await backend._ensure_connection()
    monkeypatch.setattr(db, "execute_fetchall", boom)
    with pytest.raises(StorageUnavailable):
        await backend.load_session("any")


@pytest.mark.asyncio
async def test_load_task_corrupt_raises(backend: SQLiteBackend):
    db = await backend._ensure_connection()
    await db.execute(
        "INSERT OR REPLACE INTO tasks (id, workflow_id, status, data, created_at, updated_at) "
        "VALUES (?, ?, ?, ?, ?, ?)",
        ("t:bad", "", "pending", "{broken", "2026-01-01T00:00:00", "2026-01-01T00:00:00"),
    )
    await db.commit()
    with pytest.raises(CorruptData):
        await backend.load_task("t:bad")
    assert await backend.load_task("t:none") is None


@pytest.mark.asyncio
async def test_load_workflow_corrupt_raises(backend: SQLiteBackend):
    db = await backend._ensure_connection()
    await db.execute(
        "INSERT OR REPLACE INTO workflows (id, name, status, data, created_at, updated_at) "
        "VALUES (?, ?, ?, ?, ?, ?)",
        ("w:bad", "", "pending", "{broken", "2026-01-01T00:00:00", "2026-01-01T00:00:00"),
    )
    await db.commit()
    with pytest.raises(CorruptData):
        await backend.load_workflow("w:bad")
    assert await backend.load_workflow("w:none") is None


@pytest.mark.asyncio
async def test_load_memories_corrupt_raises(backend: SQLiteBackend):
    db = await backend._ensure_connection()
    await db.execute(
        "INSERT OR REPLACE INTO memories (id, type, key, data, created_at, updated_at) "
        "VALUES (?, ?, ?, ?, ?, ?)",
        ("m:bad", "user", "k", "{broken", "2026-01-01T00:00:00", "2026-01-01T00:00:00"),
    )
    await db.commit()
    with pytest.raises(CorruptData):
        await backend.load_memories("user")
    assert await backend.load_memories("empty_type") == []


@pytest.mark.parametrize(
    ("method_name", "args"),
    [
        ("load_session", ("session",)),
        ("list_sessions", ()),
        ("load_memories", ()),
        ("load_task", ("task",)),
        ("list_tasks", ()),
        ("load_workflow", ("workflow",)),
        ("list_workflows", ()),
        ("query_logs", ()),
        ("load_vectors_all", ()),
        ("load_vector_by_source", ("source",)),
        ("fetch_sql", ("SELECT 1",)),
        ("load_archived_messages", ("session",)),
    ],
)
@pytest.mark.parametrize(
    "error_type",
    [aiosqlite.OperationalError, OSError],
    ids=["database-error", "os-error"],
)
@pytest.mark.asyncio
async def test_public_read_apis_raise_storage_unavailable(
    backend: SQLiteBackend,
    monkeypatch: pytest.MonkeyPatch,
    method_name: str,
    args: tuple,
    error_type: type[Exception],
) -> None:
    async def unavailable():
        raise error_type("simulated read failure")

    monkeypatch.setattr(backend, "_ensure_connection", unavailable)

    with pytest.raises(StorageUnavailable, match="simulated read failure"):
        await getattr(backend, method_name)(*args)


@pytest.mark.asyncio
async def test_public_list_reads_distinguish_empty_from_failure(backend: SQLiteBackend) -> None:
    assert await backend.list_sessions() == []
    assert await backend.load_memories() == []
    assert await backend.list_tasks() == []
    assert await backend.list_workflows() == []
    assert await backend.query_logs() == []
    assert await backend.load_vectors_all() == []
    assert await backend.load_vector_by_source("missing") is None
    assert await backend.fetch_sql("SELECT key FROM sessions") == []
    assert await backend.load_archived_messages("missing") == []


@pytest.mark.asyncio
async def test_every_json_read_surface_raises_corrupt_data(backend: SQLiteBackend) -> None:
    timestamp = "2026-01-01T00:00:00"
    broken = "{broken"
    statements = [
        (
            "INSERT INTO sessions (key, data, created_at, updated_at) VALUES (?, ?, ?, ?)",
            ("bad-session", broken, timestamp, timestamp),
        ),
        (
            "INSERT INTO memories (id, type, key, data, created_at, updated_at) "
            "VALUES (?, ?, ?, ?, ?, ?)",
            ("bad-memory", "user", "key", broken, timestamp, timestamp),
        ),
        (
            "INSERT INTO tasks (id, workflow_id, status, data, created_at, updated_at) "
            "VALUES (?, ?, ?, ?, ?, ?)",
            ("bad-task", "", "pending", broken, timestamp, timestamp),
        ),
        (
            "INSERT INTO workflows (id, name, status, data, created_at, updated_at) "
            "VALUES (?, ?, ?, ?, ?, ?)",
            ("bad-workflow", "", "pending", broken, timestamp, timestamp),
        ),
        (
            "INSERT INTO logs (trace_id, data, created_at) VALUES (?, ?, ?)",
            ("bad-log", broken, timestamp),
        ),
        (
            "INSERT INTO vectors "
            "(id, source_id, embedding, metadata, model, dim, created_at) "
            "VALUES (?, ?, ?, ?, ?, ?, ?)",
            ("bad-vector", "bad-source", b"data", broken, "model", 1, timestamp),
        ),
        (
            "INSERT INTO message_archive "
            "(session_key, compression_id, messages, message_count, created_at) "
            "VALUES (?, ?, ?, ?, ?)",
            ("bad-archive", "compression", broken, 1, timestamp),
        ),
    ]
    for sql, params in statements:
        await backend.execute_sql(sql, params)

    corrupt_reads = [
        backend.list_sessions(),
        backend.load_memories(),
        backend.list_tasks(),
        backend.list_workflows(),
        backend.query_logs(),
        backend.load_vectors_all(),
        backend.load_vector_by_source("bad-source"),
        backend.load_archived_messages("bad-archive"),
    ]
    for read in corrupt_reads:
        with pytest.raises(CorruptData):
            await read


@pytest.mark.asyncio
async def test_valid_json_with_wrong_shape_is_corrupt(backend: SQLiteBackend) -> None:
    await backend.execute_sql(
        "INSERT INTO sessions (key, data, created_at, updated_at) VALUES (?, ?, ?, ?)",
        ("wrong-shape", "[]", "2026-01-01", "2026-01-01"),
    )

    with pytest.raises(CorruptData, match="expected dict"):
        await backend.list_sessions()
