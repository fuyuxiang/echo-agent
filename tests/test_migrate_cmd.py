from __future__ import annotations

import json
from pathlib import Path

from echo_agent.cli.migrate_cmd import run_migrate_command


def _seed(tmp_path: Path):
    # 造一个最小 workspace + 旧格式 user_memory.json
    mem = tmp_path / "data" / "memory"
    mem.mkdir(parents=True)
    (mem / "user_memory.json").write_text(json.dumps([{
        "id": "m1", "type": "user", "key": "user:city", "content": "上海",
        "source_session": "telegram:alice", "tags": [],
    }]), encoding="utf-8")
    # 最小配置:principal_bindings 含 telegram:alice
    cfg = tmp_path / "config.yaml"
    cfg.write_text(
        "workspace: " + str(tmp_path) + "\n"
        "memory:\n"
        "  scope_policy: session\n"
        "  owner_key: owner\n"
        "  principal_bindings:\n"
        "    - \"telegram:alice\"\n",
        encoding="utf-8",
    )
    return cfg, mem


def test_status_reports_pending(tmp_path, capsys):
    cfg, mem = _seed(tmp_path)
    rc = run_migrate_command("status", config_path=str(cfg))
    assert rc == 0
    out = capsys.readouterr().out
    assert "1" in out  # 1 条待迁移


def test_run_migrates_and_backs_up(tmp_path):
    cfg, mem = _seed(tmp_path)
    rc = run_migrate_command("run", config_path=str(cfg), yes=True)
    assert rc == 0
    data = json.loads((mem / "user_memory.json").read_text())
    assert data[0]["source_session"] == "owner"
    assert list(mem.glob("user_memory.json.migbak-*"))  # 备份已建


def test_dry_run_no_write(tmp_path):
    cfg, mem = _seed(tmp_path)
    rc = run_migrate_command("run", config_path=str(cfg), dry_run=True, yes=True)
    assert rc == 0
    data = json.loads((mem / "user_memory.json").read_text())
    assert data[0]["source_session"] == "telegram:alice"  # 未改
    assert not list(mem.glob("user_memory.json.migbak-*"))  # 未备份


def test_rollback_restores(tmp_path):
    cfg, mem = _seed(tmp_path)
    run_migrate_command("run", config_path=str(cfg), yes=True)
    rc = run_migrate_command("rollback", config_path=str(cfg), yes=True)
    assert rc == 0
    data = json.loads((mem / "user_memory.json").read_text())
    assert data[0]["source_session"] == "telegram:alice"  # 已还原
