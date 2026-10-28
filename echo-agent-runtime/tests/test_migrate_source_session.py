from __future__ import annotations

import json
import pytest

from echo_agent.memory.store import MemoryStore
from echo_agent.memory.types import MemoryEntry, MemoryType
from echo_agent.cli.migrate_cmd import (
    migrate_source_session, backup_user_memory, restore_user_memory, latest_backup,  # noqa: F401
)


def _store(tmp_path):
    return MemoryStore(memory_dir=tmp_path / "mem", scope_policy="session")


def test_migrate_rewrites_bound_source_session(tmp_path):
    s = _store(tmp_path)
    s.add(MemoryEntry(type=MemoryType.USER, key="user:city", content="上海", source_session="telegram:alice"))
    s.add(MemoryEntry(type=MemoryType.USER, key="user:job", content="工程师", source_session="discord:bob"))
    res = migrate_source_session(s, {"telegram:alice"}, "owner")
    assert res.rewritten == 1
    # 命中的改成 owner,未命中(discord:bob 不在 bindings)不动
    entries = {e.key: e.source_session for e in s.list_all(mem_type=MemoryType.USER)}
    assert entries["user:city"] == "owner"
    assert entries["user:job"] == "discord:bob"


def test_migrate_skips_global_empty_env(tmp_path):
    s = _store(tmp_path)
    s.add(MemoryEntry(type=MemoryType.USER, key="user:g", content="x", source_session="telegram:alice", tags=["global"]))
    s.add(MemoryEntry(type=MemoryType.USER, key="user:e", content="y", source_session=""))
    res = migrate_source_session(s, {"telegram:alice"}, "owner")
    assert res.rewritten == 0  # global 标签与空 source_session 都跳过


def test_migrate_idempotent(tmp_path):
    s = _store(tmp_path)
    s.add(MemoryEntry(type=MemoryType.USER, key="user:city", content="上海", source_session="telegram:alice"))
    assert migrate_source_session(s, {"telegram:alice"}, "owner").rewritten == 1
    # 第二次:已是 owner,0 条
    assert migrate_source_session(s, {"telegram:alice"}, "owner").rewritten == 0


def test_migrate_dry_run_no_write(tmp_path):
    s = _store(tmp_path)
    s.add(MemoryEntry(type=MemoryType.USER, key="user:city", content="上海", source_session="telegram:alice"))
    res = migrate_source_session(s, {"telegram:alice"}, "owner", dry_run=True)
    assert res.rewritten == 1  # 报告将改 1 条
    # 盘上未变
    raw = json.loads((tmp_path / "mem" / "user_memory.json").read_text())
    assert raw[0]["source_session"] == "telegram:alice"


def test_backup_and_restore(tmp_path):
    s = _store(tmp_path)
    s.add(MemoryEntry(type=MemoryType.USER, key="user:city", content="上海", source_session="telegram:alice"))
    mem_dir = tmp_path / "mem"
    bak = backup_user_memory(mem_dir)
    assert bak.exists()
    migrate_source_session(s, {"telegram:alice"}, "owner")
    assert json.loads((mem_dir / "user_memory.json").read_text())[0]["source_session"] == "owner"
    restore_user_memory(mem_dir)
    assert json.loads((mem_dir / "user_memory.json").read_text())[0]["source_session"] == "telegram:alice"


def test_restore_no_backup_raises(tmp_path):
    (tmp_path / "mem").mkdir(parents=True)
    with pytest.raises(FileNotFoundError):
        restore_user_memory(tmp_path / "mem")


def test_adopt_empty_dry_run_counts(tmp_path):
    # B: --adopt-empty dry-run 统计空 scope USER 条目,不写盘。
    s = _store(tmp_path)
    s.add(MemoryEntry(type=MemoryType.USER, key="user:city", content="上海", source_session=""))
    s.add(MemoryEntry(type=MemoryType.USER, key="user:g", content="x", source_session="", tags=["global"]))
    res = migrate_source_session(s, set(), "owner", dry_run=True, include_empty=True)
    assert res.adopted_empty == 1  # global 不收编
    # 盘上未变
    raw = {e["key"]: e["source_session"] for e in json.loads((tmp_path / "mem" / "user_memory.json").read_text())}
    assert raw["user:city"] == ""


def test_adopt_empty_rewrites_to_owner(tmp_path):
    # B: --adopt-empty 执行后空 scope USER 归 owner_key、对 owner 可见;global/ENV 不动。
    s = _store(tmp_path)
    s.add(MemoryEntry(type=MemoryType.USER, key="user:city", content="上海", source_session=""))
    s.add(MemoryEntry(type=MemoryType.USER, key="user:g", content="x", source_session="", tags=["global"]))
    res = migrate_source_session(s, set(), "owner", include_empty=True)
    assert res.adopted_empty == 1
    entries = {e.key: e.source_session for e in s.list_all(mem_type=MemoryType.USER)}
    assert entries["user:city"] == "owner"
    assert entries["user:g"] == ""  # global 保持空
    adopted = s.find_by_key("user:city", MemoryType.USER, session_key="owner")
    assert adopted is not None and s.is_visible_in_session(adopted, "owner")
