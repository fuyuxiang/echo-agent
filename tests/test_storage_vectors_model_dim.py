"""vectors 表 model/dim 列迁移与读写测试。"""
from pathlib import Path

import pytest
import pytest_asyncio

from echo_agent.storage.sqlite import SQLiteBackend


@pytest_asyncio.fixture
async def storage(tmp_path: Path) -> SQLiteBackend:
    backend = SQLiteBackend(tmp_path / "test.db")
    await backend.initialize()
    yield backend
    await backend.close()


@pytest.mark.asyncio
async def test_store_vector_persists_model_and_dim(storage: SQLiteBackend):
    await storage.store_vector(
        "vec1", "mem1", b"\x00" * 8, {}, model="fastembed:BAAI/bge-small-zh-v1.5", dim=512,
    )
    rows = await storage.load_vectors_all()
    assert len(rows) == 1
    assert rows[0]["model"] == "fastembed:BAAI/bge-small-zh-v1.5"
    assert rows[0]["dim"] == 512


@pytest.mark.asyncio
async def test_store_vector_defaults_empty_model_zero_dim(storage: SQLiteBackend):
    await storage.store_vector("vec2", "mem2", b"\x00" * 8, {})
    rows = await storage.load_vectors_all()
    assert rows[0]["model"] == ""
    assert rows[0]["dim"] == 0


@pytest.mark.asyncio
async def test_load_vector_by_source_includes_model_dim(storage: SQLiteBackend):
    await storage.store_vector("vec3", "mem3", b"\x00" * 8, {}, model="m", dim=4)
    row = await storage.load_vector_by_source("mem3")
    assert row is not None
    assert row["model"] == "m"
    assert row["dim"] == 4


@pytest.mark.asyncio
async def test_migration_adds_columns_to_legacy_rows(tmp_path: Path):
    """旧库升级后存量行 model=''、dim=0（视为待重嵌入）。"""
    import aiosqlite

    db_path = tmp_path / "legacy.db"
    async with aiosqlite.connect(db_path) as db:
        await db.execute(
            "CREATE TABLE vectors (id TEXT PRIMARY KEY, source_id TEXT NOT NULL, "
            "embedding BLOB, metadata TEXT, created_at TEXT NOT NULL)"
        )
        await db.execute(
            "INSERT INTO vectors VALUES ('v_old', 's_old', x'00', '{}', '2026-01-01')"
        )
        await db.execute(
            "CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL)"
        )
        await db.commit()

    backend = SQLiteBackend(db_path)
    await backend.initialize()
    rows = await backend.load_vectors_all()
    await backend.close()
    old = next(r for r in rows if r["id"] == "v_old")
    assert old["model"] == ""
    assert old["dim"] == 0
