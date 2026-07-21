from pathlib import Path
from unittest.mock import AsyncMock, MagicMock

import pytest

from echo_agent.session.manager import SessionManager
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
