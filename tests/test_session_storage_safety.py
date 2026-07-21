from pathlib import Path
from unittest.mock import AsyncMock, MagicMock

import pytest

from echo_agent.session.manager import Session, SessionManager
from echo_agent.storage.errors import StorageUnavailable, CorruptData


def _mgr(tmp_path: Path, storage) -> SessionManager:
    return SessionManager(sessions_dir=tmp_path / "sessions", storage=storage)


@pytest.mark.asyncio
async def test_get_or_create_reraises_on_storage_unavailable(tmp_path: Path):
    storage = MagicMock()
    storage.load_session = AsyncMock(side_effect=StorageUnavailable("db down"))
    mgr = _mgr(tmp_path, storage)

    with pytest.raises(StorageUnavailable):
        await mgr.get_or_create("chan:1")

    # It must NOT have written an empty session back to storage.
    storage.store_session.assert_not_called()
    assert "chan:1" not in mgr._cache


@pytest.mark.asyncio
async def test_get_or_create_reraises_on_corrupt_data(tmp_path: Path):
    storage = MagicMock()
    storage.load_session = AsyncMock(side_effect=CorruptData("bad json"))
    mgr = _mgr(tmp_path, storage)

    with pytest.raises(CorruptData):
        await mgr.get_or_create("chan:2")
    storage.store_session.assert_not_called()


@pytest.mark.asyncio
async def test_get_or_create_missing_creates_empty(tmp_path: Path):
    storage = MagicMock()
    storage.load_session = AsyncMock(return_value=None)
    mgr = _mgr(tmp_path, storage)

    session = await mgr.get_or_create("chan:3")
    assert session.key == "chan:3"
    assert session.messages == []


@pytest.mark.asyncio
async def test_sqlite_notfound_falls_back_to_downgrade_file(tmp_path: Path):
    # A real SQLite backend that has no row, plus a downgrade file on disk.
    from echo_agent.storage.sqlite import SQLiteBackend

    storage = SQLiteBackend(tmp_path / "fb.db")
    await storage.initialize()
    mgr = SessionManager(sessions_dir=tmp_path / "sessions", storage=storage)

    # Write only to the downgrade file (simulating a prior save-fallback).
    fallback = Session(key="chan:9")
    fallback.add_message("user", "from disk")
    await mgr._save_to_file(fallback)

    loaded = await mgr.get_or_create("chan:9")
    assert loaded is not None
    assert loaded.messages[0]["content"] == "from disk"
    # It should have been rewritten back into SQLite.
    assert await storage.load_session("chan:9") is not None
    await storage.close()


@pytest.mark.asyncio
async def test_storage_unavailable_does_not_read_stale_file(tmp_path: Path):
    storage = MagicMock()
    storage.load_session = AsyncMock(side_effect=StorageUnavailable("down"))
    mgr = SessionManager(sessions_dir=tmp_path / "sessions", storage=storage)

    stale = Session(key="chan:10")
    stale.add_message("user", "stale disk copy")
    await mgr._save_to_file(stale)

    # Must surface the outage, not silently serve the (possibly stale) file.
    with pytest.raises(StorageUnavailable):
        await mgr.get_or_create("chan:10")


from datetime import datetime, timedelta


@pytest.mark.asyncio
async def test_cleanup_expired_persists_status_in_file_mode(tmp_path: Path):
    mgr = SessionManager(sessions_dir=tmp_path / "sessions", expiry_hours=1)

    stale = Session(key="chan:old")
    stale.add_message("user", "hi")
    stale.updated_at = datetime.now() - timedelta(hours=48)
    await mgr._save_to_file(stale)
    await mgr.invalidate("chan:old")

    processed = await mgr.cleanup_expired()
    assert processed == 1

    # Reload from disk: the persisted status must actually be "expired".
    await mgr.invalidate("chan:old")
    reloaded = await mgr.get_or_create("chan:old")
    assert reloaded.status == "expired"
