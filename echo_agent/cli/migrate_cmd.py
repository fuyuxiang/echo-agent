from __future__ import annotations

import asyncio
import hashlib
import re
import shutil
import time
from dataclasses import dataclass, field
from pathlib import Path

from loguru import logger

from echo_agent.memory.types import MemoryType

# 渲染视图(render.py)的事实行:`- **key** [tags]: content`。tags 段可选。
_RENDER_LINE = re.compile(r"^-\s+\*\*(?P<key>[^*]+)\*\*(?:\s+\[(?P<tags>[^\]]*)\])?:\s*(?P<content>.+)$")

_BACKUP_PREFIX = "user_memory.json.migbak-"


@dataclass
class MigrationResult:
    rewritten: int = 0
    old_keys: list[str] = field(default_factory=list)
    skipped: int = 0
    adopted_empty: int = 0


def migrate_source_session(
    store, bindings, owner_key: str, dry_run: bool = False, include_empty: bool = False,
) -> MigrationResult:
    """把 source_session 精确命中 bindings 的 USER 条目改写为 owner_key。
    跳过:空 source_session、含 global tag、已是 owner_key。ENVIRONMENT 天然不在 USER 列表。
    include_empty=True 时,额外把空 source_session 且不含 global tag 的 USER 条目也收编为
    owner_key(配合 store 可见性 fail-closed:未收编的空 scope USER 记忆将不可见但不丢失)。
    dry_run=True 只统计不写盘。不合并同 key。"""
    result = MigrationResult()
    hit_ids: list[str] = []
    for entry in store.list_all(mem_type=MemoryType.USER):
        ss = entry.source_session or ""
        if not ss:
            # 空 scope:仅在 include_empty 且非 global 时收编,否则按原逻辑跳过。
            if include_empty and "global" not in entry.tags:
                result.adopted_empty += 1
                if not dry_run:
                    entry.source_session = owner_key
                    hit_ids.append(entry.id)
            continue
        if "global" in entry.tags or ss == owner_key:
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


def backup_user_memory(memory_dir: Path) -> "Path | None":
    src = memory_dir / "user_memory.json"
    if not src.exists():
        # 首次迁移(如仅导入 MEMORY.md 分片)时 user_memory.json 尚不存在:
        # 不再让 shutil.copy2 抛 FileNotFoundError,返回 None 表示无需备份。
        print("user_memory.json 尚不存在,跳过备份")
        return None
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


def parse_render_view(text: str) -> list[tuple[str, list[str], str]]:
    """解析 render.py 渲染视图,抽出 (key, tags, content) 三元组。
    只认 `- **key** [t1, t2]: content` 事实行,`## 分组`/空行/其它文本忽略。"""
    facts: list[tuple[str, list[str], str]] = []
    for line in text.splitlines():
        m = _RENDER_LINE.match(line.strip())
        if not m:
            continue
        key = m.group("key").strip()
        raw_tags = m.group("tags") or ""
        tags = [t.strip() for t in raw_tags.split(",") if t.strip()]
        content = m.group("content").strip()
        if key and content:
            facts.append((key, tags, content))
    return facts


def _scope_digest(scope: str) -> str:
    """与 store._long_term_path 完全一致的短哈希:sha256(scope 或空串).hexdigest()[:8]。"""
    return hashlib.sha256((scope or "").encode("utf-8")).hexdigest()[:8]


def _shard_hash(path: Path) -> "str | None":
    """取分片文件名末段的 8 位十六进制短哈希。`MEMORY.<safe_scope>.<digest>.md`→digest;
    旧全局 `MEMORY.md` 或无哈希段→None。"""
    parts = path.name.split(".")
    if parts and parts[-1] == "md":
        parts = parts[:-1]
    if len(parts) >= 3 and re.fullmatch(r"[0-9a-f]{8}", parts[-1]):
        return parts[-1]
    return None


def _scope_from_shard(
    path: Path, candidates: "set[str] | None" = None, legacy_scope: str = "default",
) -> "str | None":
    """靠短哈希反查真实 scope,而非用文件名里的净化串。净化(: / → _)不可逆,
    直接用净化串会让迁移条目 source_session 与读侧真实会话键恒不等、永久不可见。
    对每个候选 scope 算 sha256 短哈希与文件名末段比对,命中即为真实 scope。
    旧全局 MEMORY.md(无哈希段)→ legacy_scope(默认 default);查不到匹配 → None
    (调用方跳过,绝不写净化串)。"""
    digest = _shard_hash(path)
    if digest is None:
        # 旧全局 MEMORY.md 无哈希段:映射到 legacy_scope。调用方应传 owner_key,
        # 否则用 default(空 scope 会被 USER 写口拒 → owner 永远看不见,"软失忆")。
        return legacy_scope
    for cand in candidates or set():
        if _scope_digest(cand) == digest:
            return cand
    return None


def _build_scope_candidates(store, owner_key: str) -> "set[str]":
    """构造短哈希反查候选:store 现有条目的 source_session + owner_key + 空串。
    覆盖迁移前分片实际写入用过的作用域,足以反查回真实 scope。"""
    candidates = {e.source_session for e in store._entries.values() if e.source_session}
    candidates.add(owner_key or "owner")
    candidates.add("")
    return candidates


async def _import_memory_md(store, memory_dir: Path, dry_run: bool, owner_key: str = "owner") -> int:
    """扫描 memory_dir 下 MEMORY.*.md 分片(及旧全局 MEMORY.md),把渲染视图里的
    事实行经 service.promote 入 store,成功入库(stored>0)后原分片改名 .imported。
    scope 靠短哈希反查真实值;反查不到则跳过该分片并告警,绝不用净化串写坏 scope。
    旧全局 MEMORY.md(无哈希段)映射到 owner_key,归 owner 可见。
    分片是 USER 渲染视图,条目一律按 USER 导入——强制 USER 是设计而非缺陷。
    返回入库条目数。"""
    from echo_agent.memory.service import ActorContext, MemoryService

    service = MemoryService(store)
    candidates = _build_scope_candidates(store, owner_key)
    shards = sorted(memory_dir.glob("MEMORY.*.md"))
    legacy = memory_dir / "MEMORY.md"
    if legacy.exists():
        shards.append(legacy)

    imported = 0
    for shard in shards:
        if shard.name.endswith(".imported"):
            continue
        scope = _scope_from_shard(shard, candidates, legacy_scope=owner_key)
        if scope is None:
            logger.warning(f"无法还原 scope,跳过 {shard.name}")
            print(f"跳过(无法还原 scope): {shard.name}")
            continue
        text = shard.read_text(encoding="utf-8")
        facts = parse_render_view(text)
        if not facts:
            print(f"跳过(无可解析事实): {shard.name}")
            continue
        if dry_run:
            print(f"[dry-run] {shard.name}(scope={scope}): 将入库 {len(facts)} 条")
            for key, _tags, _content in facts:
                print(f"[dry-run]   - {key}")
            continue
        ctx = ActorContext(actor="migration", session_key=scope, memory_scope=scope)
        stored = 0
        for key, tags, content in facts:
            res = await service.promote(
                ctx,
                type=MemoryType.USER,
                key=key,
                content=content,
                tags=tags,
                importance=0.5,
                source="consolidated",
            )
            if res.ok:
                stored += 1
            else:
                print(f"  条目 {key} 未入库: {res.reason}")
        imported += stored
        # 仅在确有条目入库时改名,全失败/0 条不动原分片,便于重试。
        if stored > 0:
            shard.rename(shard.with_name(shard.name + ".imported"))
            print(f"已入库 {stored} 条并备份为 {shard.name}.imported(scope={scope})")
        else:
            print(f"未入库任何条目,保留原分片: {shard.name}(scope={scope})")
    return imported


def run_migrate_command(
    action, *, config_path=None, workspace=None, dry_run=False, yes=False, adopt_empty=False,
) -> int:
    from echo_agent.cli.plugins_cmd import _get_config_and_workspace

    config, ws = _get_config_and_workspace(config_path, workspace)
    memory_dir = ws / config.storage.memory_dir
    bindings = set(config.memory.principal_bindings)
    owner_key = config.memory.owner_key
    store = _load_store(config, ws)

    if action == "status":
        res = migrate_source_session(store, bindings, owner_key, dry_run=True, include_empty=True)
        bak = latest_backup(memory_dir)
        print(f"待迁移(命中 bindings 但仍是旧键)的 USER 条目: {res.rewritten}")
        print(f"空 scope 待收编(需 --adopt-empty)的 USER 条目: {res.adopted_empty}")
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
            res = migrate_source_session(
                store, bindings, owner_key, dry_run=True, include_empty=adopt_empty
            )
            print(f"[dry-run] 将改写 {res.rewritten} 条 USER 记忆的 source_session→{owner_key}")
            print(f"[dry-run] 涉及旧键: {sorted(set(res.old_keys))}")
            if adopt_empty:
                print(f"[dry-run] 将收编 {res.adopted_empty} 条空 scope USER 记忆→{owner_key}")
            return 0
        if not yes:
            extra = "(含空 scope 收编)" if adopt_empty else ""
            reply = input(
                f"将改写命中 bindings 的 USER 记忆 source_session→{owner_key}{extra}"
                "(先自动备份),继续? [y/N] "
            )
            if reply.strip().lower() != "y":
                print("已取消")
                return 1
        backup_user_memory(memory_dir)
        res = migrate_source_session(store, bindings, owner_key, include_empty=adopt_empty)
        print(f"已迁移 {res.rewritten} 条,涉及旧键: {sorted(set(res.old_keys))}")
        if adopt_empty:
            print(f"已收编 {res.adopted_empty} 条空 scope USER 记忆→{owner_key}")
        return 0

    if action == "memory-md":
        if not dry_run and not yes:
            reply = input("将把存量 MEMORY.*.md 分片抽取入 store(先自动备份 user_memory.json),继续? [y/N] ")
            if reply.strip().lower() != "y":
                print("已取消")
                return 1
        if not dry_run:
            backup_user_memory(memory_dir)
        total = asyncio.run(_import_memory_md(store, memory_dir, dry_run, owner_key))
        if dry_run:
            print("[dry-run] 未写 store、未改名")
        else:
            print(f"共入库 {total} 条")
        return 0

    print(f"未知 action: {action}")
    return 1
