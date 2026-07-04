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
