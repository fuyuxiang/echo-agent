from __future__ import annotations

import shutil
import time
from dataclasses import dataclass, field
from pathlib import Path

from echo_agent.memory.types import MemoryType

_BACKUP_PREFIX = "user_memory.json.migbak-"


@dataclass
class MigrationResult:
    rewritten: int = 0
    old_keys: list[str] = field(default_factory=list)
    skipped: int = 0


def migrate_source_session(store, bindings, owner_key: str, dry_run: bool = False) -> MigrationResult:
    """把 source_session 精确命中 bindings 的 USER 条目改写为 owner_key。
    跳过:空 source_session、含 global tag、已是 owner_key。ENVIRONMENT 天然不在 USER 列表。
    dry_run=True 只统计不写盘。不合并同 key。"""
    result = MigrationResult()
    hit_ids: list[str] = []
    for entry in store.list_all(mem_type=MemoryType.USER):
        ss = entry.source_session or ""
        if not ss or "global" in entry.tags or ss == owner_key:
            continue
        if ss not in bindings:
            result.skipped += 1
            continue
        result.rewritten += 1
        result.old_keys.append(ss)
        if not dry_run:
            entry.source_session = owner_key
            hit_ids.append(entry.id)
    if not dry_run and hit_ids:
        store._dirty_ids.update(hit_ids)
        store._save_type(MemoryType.USER)
    return result


def backup_user_memory(memory_dir: Path) -> Path:
    src = memory_dir / "user_memory.json"
    dst = memory_dir / f"{_BACKUP_PREFIX}{int(time.time())}"
    shutil.copy2(src, dst)
    return dst


def latest_backup(memory_dir: Path) -> "Path | None":
    baks = sorted(memory_dir.glob(f"{_BACKUP_PREFIX}*"))
    return baks[-1] if baks else None


def restore_user_memory(memory_dir: Path) -> Path:
    bak = latest_backup(memory_dir)
    if bak is None:
        raise FileNotFoundError(f"no migration backup found in {memory_dir}")
    shutil.copy2(bak, memory_dir / "user_memory.json")
    return bak


def _load_store(config, workspace: Path):
    from echo_agent.memory.store import MemoryStore
    return MemoryStore(
        memory_dir=workspace / config.storage.memory_dir,
        scope_policy=config.memory.scope_policy,
    )


def run_migrate_command(action, *, config_path=None, workspace=None, dry_run=False, yes=False) -> int:
    from echo_agent.cli.plugins_cmd import _get_config_and_workspace

    config, ws = _get_config_and_workspace(config_path, workspace)
    memory_dir = ws / config.storage.memory_dir
    bindings = set(config.memory.principal_bindings)
    owner_key = config.memory.owner_key
    store = _load_store(config, ws)

    if action == "status":
        res = migrate_source_session(store, bindings, owner_key, dry_run=True)
        bak = latest_backup(memory_dir)
        print(f"待迁移(命中 bindings 但仍是旧键)的 USER 条目: {res.rewritten}")
        print(f"迁移备份: {bak.name if bak else '无'}")
        return 0

    if action == "rollback":
        if not yes:
            reply = input("回滚将用最近备份覆盖 user_memory.json,继续? [y/N] ")
            if reply.strip().lower() != "y":
                print("已取消")
                return 1
        try:
            bak = restore_user_memory(memory_dir)
        except FileNotFoundError as e:
            print(f"错误: {e}")
            return 1
        print(f"已从 {bak.name} 回滚")
        return 0

    if action == "run":
        if dry_run:
            res = migrate_source_session(store, bindings, owner_key, dry_run=True)
            print(f"[dry-run] 将改写 {res.rewritten} 条 USER 记忆的 source_session→{owner_key}")
            print(f"[dry-run] 涉及旧键: {sorted(set(res.old_keys))}")
            return 0
        if not yes:
            reply = input(f"将改写命中 bindings 的 USER 记忆 source_session→{owner_key}(先自动备份),继续? [y/N] ")
            if reply.strip().lower() != "y":
                print("已取消")
                return 1
        backup_user_memory(memory_dir)
        res = migrate_source_session(store, bindings, owner_key)
        print(f"已迁移 {res.rewritten} 条,涉及旧键: {sorted(set(res.old_keys))}")
        return 0

    print(f"未知 action: {action}")
    return 1
