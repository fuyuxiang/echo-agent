import shutil
from pathlib import Path

import pytest

from echo_agent.cli.checkpoint_cmd import _build_manager, run_checkpoint_command

pytestmark = pytest.mark.skipif(shutil.which("git") is None, reason="git not installed")


@pytest.mark.asyncio
async def test_build_manager_and_list_empty(tmp_path: Path):
    mgr = _build_manager(str(tmp_path / "store"), tmp_path / "ws")
    assert await mgr.list_snapshots() == []


def test_run_checkpoint_list_smoke(tmp_path: Path, capsys, monkeypatch):
    ws = tmp_path / "ws"
    ws.mkdir()
    store_path = str(tmp_path / "store")

    # bypass real config: patch the loader helper to return our paths
    import echo_agent.cli.checkpoint_cmd as mod
    monkeypatch.setattr(mod, "_resolve_store_and_ws",
                        lambda cp, w: (store_path, ws))
    run_checkpoint_command("list", config_path=None, workspace=str(ws))
    out = capsys.readouterr().out
    assert "No checkpoints" in out or "checkpoint" in out.lower()


def test_restore_abort_when_user_declines(tmp_path: Path, capsys, monkeypatch):
    ws = tmp_path / "ws"
    ws.mkdir()
    store_path = str(tmp_path / "store")

    import echo_agent.cli.checkpoint_cmd as mod
    monkeypatch.setattr(mod, "_resolve_store_and_ws",
                        lambda cp, w: (store_path, ws))
    monkeypatch.setattr("builtins.input", lambda _prompt: "n")

    called = False
    orig_restore = mod.CheckpointManager.restore

    async def _spy_restore(self, sha):  # pragma: no cover - should not run
        nonlocal called
        called = True
        return await orig_restore(self, sha)

    monkeypatch.setattr(mod.CheckpointManager, "restore", _spy_restore)

    run_checkpoint_command("restore", sha="deadbeef", config_path=None, workspace=str(ws))
    out = capsys.readouterr().out
    assert "Aborted." in out
    assert called is False


def test_show_invalid_sha_prints_friendly_error(tmp_path: Path, capsys):
    ws = tmp_path / "ws"
    ws.mkdir()
    store_path = str(tmp_path / "store")

    import echo_agent.cli.checkpoint_cmd as mod
    orig = mod._resolve_store_and_ws
    mod._resolve_store_and_ws = lambda cp, w: (store_path, ws)
    try:
        with pytest.raises(SystemExit) as exc:
            run_checkpoint_command("show", sha="deadbeef", config_path=None, workspace=str(ws))
    finally:
        mod._resolve_store_and_ws = orig
    assert exc.value.code == 1
    out = capsys.readouterr().out
    assert "错误:" in out


def test_restore_invalid_sha_prints_friendly_error(tmp_path: Path, capsys):
    ws = tmp_path / "ws"
    ws.mkdir()
    store_path = str(tmp_path / "store")

    import echo_agent.cli.checkpoint_cmd as mod
    orig = mod._resolve_store_and_ws
    mod._resolve_store_and_ws = lambda cp, w: (store_path, ws)
    try:
        with pytest.raises(SystemExit) as exc:
            run_checkpoint_command(
                "restore", sha="deadbeef", config_path=None, workspace=str(ws), yes=True
            )
    finally:
        mod._resolve_store_and_ws = orig
    assert exc.value.code == 1
    out = capsys.readouterr().out
    assert "错误:" in out
