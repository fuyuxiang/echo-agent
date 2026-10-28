"""Offline memory maintenance must not race an active workspace runtime."""

import json

import pytest

from echo_agent.cli.migrate_cmd import run_migrate_command
from echo_agent.runtime_lock import acquire_instance_lock


@pytest.mark.parametrize("action", ["run", "rollback", "memory-md"])
def test_memory_write_refuses_live_runtime_without_changing_files(tmp_path, action, capsys):
    config = tmp_path / "echo-agent.yaml"
    config.write_text(
        f"workspace: {tmp_path}\nmemory:\n  principal_bindings: ['telegram:alice']\n"
    )
    memory = tmp_path / "data/memory"
    memory.mkdir(parents=True)
    current = memory / "user_memory.json"
    current.write_text(json.dumps([{
        "id": "m1", "type": "user", "key": "city", "content": "Shanghai",
        "source_session": "telegram:alice", "tags": [],
    }]))
    (memory / "user_memory.json.migbak-1").write_text("[]")
    (memory / "MEMORY.md").write_text("- **hobby**: hiking\n")
    original = {p.name: p.read_bytes() for p in memory.iterdir()}
    lock = acquire_instance_lock(tmp_path, role="test gateway")
    try:
        assert run_migrate_command(action, config_path=str(config), yes=True) == 1
        assert {p.name: p.read_bytes() for p in memory.iterdir()} == original
        assert "停止" in capsys.readouterr().out
    finally:
        lock.release()


def test_migration_releases_workspace_lock_after_failure(tmp_path, monkeypatch):
    config = tmp_path / "echo-agent.yaml"
    config.write_text(f"workspace: {tmp_path}\n")

    def fail_store(*args):
        raise RuntimeError("simulated broken store")

    monkeypatch.setattr("echo_agent.cli.migrate_cmd._load_store", fail_store)
    with pytest.raises(RuntimeError, match="broken store"):
        run_migrate_command("run", config_path=str(config), yes=True)
    lock = acquire_instance_lock(tmp_path, role="next gateway")
    lock.release()
