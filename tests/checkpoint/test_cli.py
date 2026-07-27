import shutil
from pathlib import Path
from types import SimpleNamespace

import pytest

from echo_agent.cli.checkpoint_cmd import _build_manager, run_checkpoint_command

pytestmark = pytest.mark.skipif(shutil.which("git") is None, reason="git not installed")


def _fake_config(store_path):
    """Minimal stand-in for the loaded config the CLI helpers consume."""
    return SimpleNamespace(
        checkpoint=SimpleNamespace(store_path=store_path),
        storage=SimpleNamespace(
            database_path="data/echo_agent.db",
            sessions_dir="data/sessions",
            memory_dir="data/memory",
            logs_dir="data/logs",
        ),
    )


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
    monkeypatch.setattr(mod, "_resolve_config_and_ws",
                        lambda cp, w: (_fake_config(store_path), ws))
    run_checkpoint_command("list", config_path=None, workspace=str(ws))
    out = capsys.readouterr().out
    assert "No checkpoints" in out or "checkpoint" in out.lower()


def test_restore_abort_when_user_declines(tmp_path: Path, capsys, monkeypatch):
    ws = tmp_path / "ws"
    ws.mkdir()
    store_path = str(tmp_path / "store")

    import echo_agent.cli.checkpoint_cmd as mod
    monkeypatch.setattr(mod, "_resolve_config_and_ws",
                        lambda cp, w: (_fake_config(store_path), ws))
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
    orig = mod._resolve_config_and_ws
    mod._resolve_config_and_ws = lambda cp, w: (_fake_config(store_path), ws)
    try:
        # 退出码由 run_checkpoint_command 返回,sys.exit 归 __main__ 统一负责。
        rc = run_checkpoint_command("show", sha="deadbeef", config_path=None, workspace=str(ws))
    finally:
        mod._resolve_config_and_ws = orig
    assert rc == 1
    out = capsys.readouterr().out
    assert "错误:" in out


def test_restore_invalid_sha_prints_friendly_error(tmp_path: Path, capsys):
    ws = tmp_path / "ws"
    ws.mkdir()
    store_path = str(tmp_path / "store")

    import echo_agent.cli.checkpoint_cmd as mod
    orig = mod._resolve_config_and_ws
    mod._resolve_config_and_ws = lambda cp, w: (_fake_config(store_path), ws)
    try:
        rc = run_checkpoint_command(
            "restore", sha="deadbeef", config_path=None, workspace=str(ws), yes=True
        )
    finally:
        mod._resolve_config_and_ws = orig
    assert rc == 1
    out = capsys.readouterr().out
    assert "错误:" in out
