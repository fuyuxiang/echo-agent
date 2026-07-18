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
