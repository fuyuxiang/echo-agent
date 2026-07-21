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
