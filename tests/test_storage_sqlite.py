"""Tests for async SQLite storage backend."""

from __future__ import annotations

import asyncio
from pathlib import Path

import pytest
import pytest_asyncio

from echo_agent.storage.sqlite import SQLiteBackend
from echo_agent.session.manager import SessionManager


@pytest_asyncio.fixture
async def backend(tmp_path: Path) -> SQLiteBackend:
    db = SQLiteBackend(tmp_path / "test.db")
    await db.initialize()
    yield db
    await db.close()


@pytest.mark.asyncio
async def test_initialize_creates_db(tmp_path: Path) -> None:
    db = SQLiteBackend(tmp_path / "sub" / "test.db")
    await db.initialize()
    assert db._db is not None
    await db.close()
    assert db._db is None


@pytest.mark.asyncio
async def test_store_and_load_session(backend: SQLiteBackend) -> None:
    data = {"messages": [{"role": "user", "content": "hi"}], "status": "active"}
    await backend.store_session("test:1", data)
    loaded = await backend.load_session("test:1")
    assert loaded is not None
    assert loaded["messages"][0]["content"] == "hi"


@pytest.mark.asyncio
async def test_list_sessions_returns_storage_metadata(backend: SQLiteBackend) -> None:
    await backend.store_session(
        "test:1",
        {
            "messages": [{"role": "user", "content": "hi"}],
            "status": "active",
            "metadata": {"channel": "test"},
        },
    )

    sessions = await backend.list_sessions()

    assert sessions[0]["key"] == "test:1"
    assert sessions[0]["status"] == "active"
    assert sessions[0]["metadata"] == {"channel": "test"}
    assert sessions[0]["message_count"] == 1


@pytest.mark.asyncio
async def test_session_manager_archives_storage_session_without_deleting(tmp_path: Path, backend: SQLiteBackend) -> None:
    manager = SessionManager(sessions_dir=tmp_path / "sessions", storage=backend)
    session = await manager.get_or_create("store:1")
    session.add_message("user", "persist me")
    await manager.save(session)

    assert await manager.archive_session("store:1") is True
    assert await manager.archive_session("missing:1") is False

    loaded = await backend.load_session("store:1")
    listed = await manager.list_sessions_async()

    assert loaded is not None
    assert loaded["status"] == "archived"
    assert listed[0]["key"] == "store:1"
    assert listed[0]["status"] == "archived"


@pytest.mark.asyncio
async def test_load_missing_session(backend: SQLiteBackend) -> None:
    result = await backend.load_session("nonexistent")
    assert result is None


@pytest.mark.asyncio
async def test_delete_session(backend: SQLiteBackend) -> None:
    await backend.store_session("del:1", {"messages": []})
    assert await backend.delete_session("del:1") is True
    assert await backend.load_session("del:1") is None
    assert await backend.delete_session("del:1") is False


@pytest.mark.asyncio
async def test_store_and_load_memory(backend: SQLiteBackend) -> None:
    data = {"type": "user", "key": "name", "content": "test"}
    await backend.store_memory("m1", data)
    memories = await backend.load_memories("user")
    assert len(memories) == 1
    assert memories[0]["key"] == "name"


@pytest.mark.asyncio
async def test_delete_memory(backend: SQLiteBackend) -> None:
    await backend.store_memory("m2", {"type": "env", "key": "k"})
    assert await backend.delete_memory("m2") is True
    assert await backend.delete_memory("m2") is False


@pytest.mark.asyncio
async def test_store_and_load_task(backend: SQLiteBackend) -> None:
    await backend.store_task("t1", {"status": "pending", "workflow_id": "w1"})
    task = await backend.load_task("t1")
    assert task is not None
    assert task["status"] == "pending"


@pytest.mark.asyncio
async def test_list_tasks_with_filters(backend: SQLiteBackend) -> None:
    await backend.store_task("t1", {"status": "pending", "workflow_id": "w1"})
    await backend.store_task("t2", {"status": "done", "workflow_id": "w1"})
    await backend.store_task("t3", {"status": "pending", "workflow_id": "w2"})

    all_tasks = await backend.list_tasks()
    assert len(all_tasks) == 3

    w1_tasks = await backend.list_tasks(workflow_id="w1")
    assert len(w1_tasks) == 2

    pending = await backend.list_tasks(status="pending")
    assert len(pending) == 2


@pytest.mark.asyncio
async def test_reconnect_after_close(tmp_path: Path) -> None:
    db = SQLiteBackend(tmp_path / "reconnect.db")
    await db.initialize()
    await db.store_session("k", {"messages": []})

    await db._db.close()
    db._db = None

    loaded = await db.load_session("k")
    assert loaded is not None
    await db.close()


@pytest.mark.asyncio
async def test_concurrent_writes(backend: SQLiteBackend) -> None:
    async def write(i: int) -> None:
        await backend.store_session(f"c:{i}", {"messages": [{"i": i}]})

    await asyncio.gather(*(write(i) for i in range(20)))

    for i in range(20):
        loaded = await backend.load_session(f"c:{i}")
        assert loaded is not None
