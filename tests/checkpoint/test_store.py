# tests/checkpoint/test_store.py
import shutil
from pathlib import Path

import pytest

from echo_agent.checkpoint import git_available
from echo_agent.checkpoint.store import ShadowGitStore

pytestmark = pytest.mark.skipif(shutil.which("git") is None, reason="git not installed")


def test_git_available_true_when_git_on_path():
    assert git_available() is True


@pytest.mark.asyncio
async def test_ensure_initialized_creates_bare_store(tmp_path: Path):
    store = ShadowGitStore(tmp_path / "store")
    await store.ensure_initialized()
    assert (tmp_path / "store").exists()
    # git objects dir proves it is a real repo
    assert (tmp_path / "store" / "objects").exists()


def test_workspace_hash_is_stable_and_distinct(tmp_path: Path):
    store = ShadowGitStore(tmp_path / "store")
    ws_a = tmp_path / "a"
    ws_b = tmp_path / "b"
    assert store._workspace_hash(ws_a) == store._workspace_hash(ws_a)
    assert store._workspace_hash(ws_a) != store._workspace_hash(ws_b)
    assert store.ref_for(ws_a).startswith("refs/echo/")


def test_env_for_isolates_from_user_git(tmp_path: Path):
    store = ShadowGitStore(tmp_path / "store")
    ws = tmp_path / "ws"
    ws.mkdir()
    env = store._env_for(ws)
    assert env["GIT_DIR"] == str((tmp_path / "store").resolve())
    assert env["GIT_WORK_TREE"] == str(ws.resolve())
    assert "GIT_INDEX_FILE" in env
    # index file is per-workspace, lives inside the store, not the workspace
    assert str((tmp_path / "store").resolve()) in env["GIT_INDEX_FILE"]


@pytest.mark.asyncio
async def test_take_snapshot_commits_and_returns_hash(tmp_path: Path):
    store = ShadowGitStore(tmp_path / "store")
    await store.ensure_initialized()
    ws = tmp_path / "ws"
    ws.mkdir()
    (ws / "a.txt").write_text("hello")
    sha = await store.take_snapshot(ws, "before write_file")
    assert sha and len(sha) >= 7
    # ref now points at the commit
    rc, out, _ = await store._run_git(["rev-parse", store.ref_for(ws)], check=False)
    assert out.strip() == sha


@pytest.mark.asyncio
async def test_take_snapshot_skips_when_no_change(tmp_path: Path):
    store = ShadowGitStore(tmp_path / "store")
    await store.ensure_initialized()
    ws = tmp_path / "ws"
    ws.mkdir()
    (ws / "a.txt").write_text("hello")
    first = await store.take_snapshot(ws, "first")
    assert first is not None
    second = await store.take_snapshot(ws, "no change")
    assert second is None


@pytest.mark.asyncio
async def test_take_snapshot_excludes_oversize_file(tmp_path: Path):
    store = ShadowGitStore(tmp_path / "store")
    await store.ensure_initialized()
    ws = tmp_path / "ws"
    ws.mkdir()
    (ws / "small.txt").write_text("ok")
    (ws / "big.bin").write_bytes(b"x" * (2 * 1024 * 1024))
    sha = await store.take_snapshot(ws, "with big", max_file_size_mb=1)
    assert sha is not None
    rc, out, _ = await store._run_git(
        ["ls-tree", "-r", "--name-only", sha], workspace=ws, check=False
    )
    names = set(out.split())
    assert "small.txt" in names
    assert "big.bin" not in names


@pytest.mark.asyncio
async def test_take_snapshot_excludes_oversize_unicode_file(tmp_path: Path):
    store = ShadowGitStore(tmp_path / "store")
    await store.ensure_initialized()
    ws = tmp_path / "ws"
    ws.mkdir()
    (ws / "小文件.txt").write_text("ok")
    (ws / "大文件.bin").write_bytes(b"x" * (2 * 1024 * 1024))
    sha = await store.take_snapshot(ws, "中文超大文件", max_file_size_mb=1)
    assert sha is not None
    rc, out, _ = await store._run_git(
        ["ls-tree", "-r", "--name-only", "-z", sha], workspace=ws, check=False
    )
    names = {n for n in out.split("\x00") if n.strip()}
    assert "小文件.txt" in names
    assert "大文件.bin" not in names


@pytest.mark.asyncio
async def test_list_and_changed_paths(tmp_path: Path):
    store = ShadowGitStore(tmp_path / "store")
    await store.ensure_initialized()
    ws = tmp_path / "ws"
    ws.mkdir()
    (ws / "a.txt").write_text("v1")
    s1 = await store.take_snapshot(ws, "first")
    (ws / "a.txt").write_text("v2")
    (ws / "b.txt").write_text("new")
    s2 = await store.take_snapshot(ws, "second")
    snaps = await store.list_snapshots(ws)
    assert [s["sha"] for s in snaps][:2] == [s2, s1]
    changed = await store.changed_paths(ws, s2)
    assert set(changed) == {"a.txt", "b.txt"}


@pytest.mark.asyncio
async def test_restore_only_touches_changed_files(tmp_path: Path):
    store = ShadowGitStore(tmp_path / "store")
    await store.ensure_initialized()
    ws = tmp_path / "ws"
    ws.mkdir()
    (ws / "a.txt").write_text("v1")
    s1 = await store.take_snapshot(ws, "first")
    (ws / "a.txt").write_text("v2-broken")
    await store.take_snapshot(ws, "second")
    # user later creates an unrelated file after the snapshot we roll back to
    (ws / "unrelated.txt").write_text("keep me")
    restored = await store.restore(ws, s1)
    assert (ws / "a.txt").read_text() == "v1"
    assert restored == ["a.txt"]
    # unrelated file untouched
    assert (ws / "unrelated.txt").read_text() == "keep me"


@pytest.mark.asyncio
async def test_restore_rejects_foreign_sha(tmp_path: Path):
    store = ShadowGitStore(tmp_path / "store")
    await store.ensure_initialized()
    ws = tmp_path / "ws"
    ws.mkdir()
    (ws / "a.txt").write_text("v1")
    await store.take_snapshot(ws, "first")
    with pytest.raises(ValueError):
        await store.restore(ws, "0" * 40)


@pytest.mark.asyncio
async def test_restore_handles_unicode_filename(tmp_path: Path):
    store = ShadowGitStore(tmp_path / "store")
    await store.ensure_initialized()
    ws = tmp_path / "ws"
    ws.mkdir()
    (ws / "报告.txt").write_text("v1")
    s1 = await store.take_snapshot(ws, "first")
    (ws / "报告.txt").write_text("v2-broken")
    await store.take_snapshot(ws, "second")
    restored = await store.restore(ws, s1)
    assert "报告.txt" in restored
    assert (ws / "报告.txt").read_text() == "v1"


@pytest.mark.asyncio
async def test_prune_keeps_only_max_snapshots(tmp_path: Path):
    store = ShadowGitStore(tmp_path / "store")
    await store.ensure_initialized()
    ws = tmp_path / "ws"
    ws.mkdir()
    for i in range(5):
        (ws / "a.txt").write_text(f"v{i}")
        await store.take_snapshot(ws, f"snap {i}")
    before = {s["subject"]: s["ts"] for s in await store.list_snapshots(ws)}
    dropped = await store.prune(ws, max_snapshots=2)
    assert dropped == 3
    snaps = await store.list_snapshots(ws)
    assert len(snaps) == 2
    # re-root must preserve each kept snapshot's original ts, not collapse them
    # all to the prune moment.
    for s in snaps:
        assert s["ts"] == before[s["subject"]]


@pytest.mark.asyncio
async def test_total_size_mb_positive_after_snapshot(tmp_path: Path):
    store = ShadowGitStore(tmp_path / "store")
    await store.ensure_initialized()
    ws = tmp_path / "ws"
    ws.mkdir()
    (ws / "a.txt").write_text("hello")
    await store.take_snapshot(ws, "s")
    assert await store.total_size_mb() > 0


# ── Regression: 工单 ① checkpoint restore with deleted files / exclude ────


@pytest.mark.asyncio
async def test_restore_with_deleted_file(tmp_path: Path):
    """P0: a file DELETED between snapshots must not crash restore.

    The deleted file is absent from the target tree, so the old
    `git checkout sha -- <file>` aborted the whole restore with
    'pathspec did not match'. Restoring to the target now removes it.
    """
    store = ShadowGitStore(tmp_path / "store")
    await store.ensure_initialized()
    ws = tmp_path / "ws"
    ws.mkdir()
    (ws / "a.txt").write_text("v1")
    (ws / "db.wal").write_text("wal-1")
    await store.take_snapshot(ws, "first")
    # second snapshot deletes db.wal and edits a.txt
    (ws / "db.wal").unlink()
    (ws / "a.txt").write_text("v2")
    s2 = await store.take_snapshot(ws, "second")
    # runtime later re-creates db.wal and edits a.txt again
    (ws / "db.wal").write_text("wal-2")
    (ws / "a.txt").write_text("v3")
    # restoring to s2 must not raise on the file deleted in s2
    restored = await store.restore(ws, s2)
    assert (ws / "a.txt").read_text() == "v2"
    # db.wal was deleted in s2 -> restore removes it from the work tree
    assert not (ws / "db.wal").exists()
    assert set(restored) == {"a.txt", "db.wal"}


@pytest.mark.asyncio
async def test_take_snapshot_excludes_runtime_paths(tmp_path: Path):
    """exclude keeps live DB dirs and the shadow store's own dir out of snapshots."""
    store = ShadowGitStore(
        tmp_path / "store",
        exclude=("data/", "checkpoints/", "*.db-wal"),
    )
    await store.ensure_initialized()
    ws = tmp_path / "ws"
    ws.mkdir()
    (ws / "keep.txt").write_text("keep")
    (ws / "data").mkdir()
    (ws / "data" / "echo_agent.db").write_text("DB")
    (ws / "checkpoints" / "store").mkdir(parents=True)
    (ws / "checkpoints" / "store" / "HEAD").write_text("ref")
    (ws / "session.db-wal").write_text("wal")
    sha = await store.take_snapshot(ws, "snap")
    assert sha is not None
    _, out, _ = await store._run_git(
        ["ls-tree", "-r", "--name-only", "-z", sha], workspace=ws, check=False
    )
    names = {n for n in out.split("\x00") if n.strip()}
    assert "keep.txt" in names
    assert not any(n.startswith("data/") for n in names)
    assert not any(n.startswith("checkpoints/") for n in names)
    assert "session.db-wal" not in names


def test_snapshot_exclude_derives_from_config(tmp_path: Path):
    """snapshot_exclude derives top-level dirs from live config + sidecar globs."""
    from types import SimpleNamespace

    from echo_agent.checkpoint.manager import snapshot_exclude

    ws = tmp_path
    config = SimpleNamespace(
        storage=SimpleNamespace(
            database_path="data/echo_agent.db",
            sessions_dir="data/sessions",
            memory_dir="data/memory",
            logs_dir="data/logs",
        ),
        checkpoint=SimpleNamespace(store_path=str(ws / "checkpoints" / "store")),
    )
    excl = snapshot_exclude(config, ws)
    assert excl == ("data/", "checkpoints/", "*.db-wal", "*.db-shm", "*.db-journal")
