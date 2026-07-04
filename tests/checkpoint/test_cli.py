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
