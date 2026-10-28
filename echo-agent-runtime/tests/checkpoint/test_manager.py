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


@pytest.mark.asyncio
async def test_transient_failure_allows_retry_same_turn(tmp_path: Path, monkeypatch):
    mgr = await _mgr(tmp_path)
    (mgr._workspace / "a.txt").write_text("v1")
    mgr.new_turn("t")

    calls = {"n": 0}
    real = mgr._store.take_snapshot

    async def flaky(*a, **k):
        calls["n"] += 1
        if calls["n"] == 1:
            raise RuntimeError("transient git error")
        return await real(*a, **k)

    monkeypatch.setattr(mgr._store, "take_snapshot", flaky)
    # first attempt fails open, turn not marked -> second attempt succeeds
    assert await mgr.ensure_checkpoint("r1") is None
    assert await mgr.ensure_checkpoint("r2") is not None


@pytest.mark.asyncio
async def test_ensure_checkpoint_returns_valid_sha_after_prune(tmp_path: Path):
    max_snapshots = 3
    mgr = await _mgr(tmp_path, max_snapshots=max_snapshots)
    last_sha: str | None = None
    for i in range(max_snapshots + 2):
        (mgr._workspace / "a.txt").write_text(f"v{i}")
        mgr.new_turn(f"turn-{i}")
        last_sha = await mgr.ensure_checkpoint(f"r{i}")
    assert last_sha is not None
    # prune re-roots kept commits; the returned sha must still be owned by the
    # ref, so show/restore accept it instead of raising via _assert_owned.
    await mgr.show(last_sha)  # must not raise
    await mgr.restore(last_sha)  # must not raise
