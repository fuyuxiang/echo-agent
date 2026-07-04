# tests/checkpoint/test_manager.py
import shutil
from pathlib import Path

import pytest

from echo_agent.checkpoint.manager import CheckpointManager
from echo_agent.checkpoint.store import ShadowGitStore

pytestmark = pytest.mark.skipif(shutil.which("git") is None, reason="git not installed")


async def _mgr(tmp_path: Path, **kw) -> CheckpointManager:
    store = ShadowGitStore(tmp_path / "store")
    await store.ensure_initialized()
    ws = tmp_path / "ws"
    ws.mkdir(exist_ok=True)
    return CheckpointManager(store=store, workspace=ws, **kw)


@pytest.mark.asyncio
async def test_ensure_checkpoint_dedups_within_turn(tmp_path: Path):
    mgr = await _mgr(tmp_path)
    (mgr._workspace / "a.txt").write_text("v1")
    mgr.new_turn("turn-1")
    first = await mgr.ensure_checkpoint("before write_file")
    assert first is not None
    second = await mgr.ensure_checkpoint("before patch")
    assert second is None  # same turn -> deduped


@pytest.mark.asyncio
async def test_new_turn_allows_fresh_snapshot(tmp_path: Path):
    mgr = await _mgr(tmp_path)
    (mgr._workspace / "a.txt").write_text("v1")
    mgr.new_turn("turn-1")
    await mgr.ensure_checkpoint("r1")
    (mgr._workspace / "a.txt").write_text("v2")
    mgr.new_turn("turn-2")
    second = await mgr.ensure_checkpoint("r2")
    assert second is not None


@pytest.mark.asyncio
async def test_disabled_manager_is_noop(tmp_path: Path):
    mgr = await _mgr(tmp_path, enabled=False)
    (mgr._workspace / "a.txt").write_text("v1")
    mgr.new_turn("t")
    assert await mgr.ensure_checkpoint("r") is None
    assert await mgr.list_snapshots() == []


@pytest.mark.asyncio
async def test_ensure_checkpoint_failopen_on_store_error(tmp_path: Path, monkeypatch):
    mgr = await _mgr(tmp_path)
    (mgr._workspace / "a.txt").write_text("v1")

    async def boom(*a, **k):
        raise RuntimeError("git exploded")

    monkeypatch.setattr(mgr._store, "take_snapshot", boom)
    mgr.new_turn("t")
    # must not raise
    assert await mgr.ensure_checkpoint("r") is None
