"""统一记忆写入口 MemoryService — 所有写操作收敛到八步写序。

R1 重构层地基:把散落在 6 个写入口各自实现的 provenance/失效/审计逻辑,
收敛成一条八步写序(校验→scope 门禁→ENV 门禁→provenance→写入→flush→失效→审计)。
本模块只包裹现有 MemoryStore 语义,不改变 store 行为;后续任务逐个把旧入口迁移到这里。

硬约束:
  - 被拒只拒绝、不写 contradiction(不打 suspected_conflict tag、不写 contradiction 行)。
  - 失效顺序:先 flush 再 invalidate。
  - maintenance/mark_superseded/set_tier 走精简写序:跳过 provenance 与 ENV 门禁
    (它们是裁决者/内部维护),但仍失效+flush+审计。
"""

from __future__ import annotations

import json
from contextlib import nullcontext
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Awaitable, Callable

from loguru import logger

from echo_agent.memory.types import (
    MemoryEntry,
    MemoryTier,
    MemoryType,
    provenance_guard,
)

_MAX_AUDIT_FILE_BYTES = 5_000_000

# ENV 门禁:model/reviewer/consolidation 三类 actor 受限(裁决者/内部维护不受限)。
# consolidation 提炼的 fact 源自 LLM 对话(不可信),模型可显式输出 type=environment
# 绕过 allow_model_environment_writes 写全局 ENV,故一并纳入门禁——默认拒,
# 管理员显式开 allow_model_environment_writes 后才可写。
_ENV_RESTRICTED_ACTORS = frozenset({"model", "reviewer", "consolidation"})

# actor → provenance 来源标签。用于没有显式 source 参数的写操作(如 remove),
# 据此对目标条目做 provenance_guard。裁决者/内部维护走精简写序、不触发此表。
_ACTOR_SOURCE = {
    "model": "model_inferred",
    "reviewer": "model_inferred",
    "reflection": "model_inferred",
    "consolidation": "consolidated",
    # admin 派生源 priority 0(不在 _SOURCE_PRIORITY),故 remove 默认被 provenance
    # 拦(对任何有来源条目),仅 override=True 显式越权——沿用止血层最安全默认。
    "admin": "admin",
    "migration": "legacy",
    "maintenance": "legacy",
}


@dataclass
class ActorContext:
    """一次写操作的发起者上下文。

    actor ∈ model/reviewer/consolidation/reflection/admin/migration/maintenance。
    session_key 为当前会话键;memory_scope 为写入目标的作用域(USER 写必填)。
    """

    actor: str
    session_key: str = ""
    memory_scope: str = ""


@dataclass
class WriteResult:
    """写操作结果。reason ∈ ""/rejected_provenance/rejected_scope/rejected_env/invalid。"""

    ok: bool
    entry: MemoryEntry | None = None
    reason: str = ""


class MemoryService:
    """记忆统一写入口:所有写操作走八步写序,集中 provenance/失效/审计。"""

    def __init__(
        self,
        store,
        *,
        invalidate_fn: Callable[[str, bool], Awaitable[Any]] | None = None,
        flush_fn: Callable[[], Awaitable[Any]] | None = None,
        audit_path: Path | None = None,
        allow_env_writes: bool = False,
    ):
        self._store = store
        self._invalidate_fn = invalidate_fn
        self._flush_fn = flush_fn
        self._audit_path = Path(audit_path) if audit_path else None
        self._allow_env_writes = allow_env_writes

    @property
    def store(self):
        """暴露底层 store 供入口做读操作(find/search/list);写操作仍须走本类。"""
        return self._store

    def _service_write(self):
        """进入 store 的 service_write 上下文(若 store 支持),使本类经过的 store
        写不触发 _service_only 软告警。store 无此能力(旧实现/测试桩)时退回 no-op。"""
        sw = getattr(self._store, "service_write", None)
        if sw is not None:
            return sw()
        return nullcontext()

    async def invalidate(self, scope: str, global_scope: bool = False) -> None:
        """公开失效钩子:供绕过八步写序、直接改 store 的裁决路径(如工具
        resolve_contradiction→ContradictionDetector.resolve→mark_superseded)显式失效。

        未注入 invalidate_fn 时安全跳过。此处不做 flush/审计——调用方是既有裁决动作,
        本方法只补回被删除的缓存失效这一环,不抢跑 detector 整体迁移(Task 7)。
        """
        if self._invalidate_fn is None:
            return
        try:
            await self._invalidate_fn(scope, global_scope)
        except Exception as e:
            logger.warning("MemoryService invalidate failed: {}", e)

    # ── 公开写 API ──────────────────────────────────────────────────────────

    async def add(
        self,
        ctx: ActorContext,
        *,
        type: MemoryType,
        key: str,
        content: str,
        tags: list[str] | None = None,
        importance: float = 0.5,
        source: str,
        pinned: bool = False,
    ) -> WriteResult:
        """新增记忆条目,走完整八步写序。新 key 无既有 target 故跳过 provenance。"""
        tags = list(tags or [])
        # ② scope 门禁
        scope_rej = self._scope_gate(type, ctx)
        if scope_rej:
            return self._reject(ctx, "add", "", type, source, scope_rej)
        # ③ ENV 门禁
        if self._env_denied(ctx.actor, type, tags):
            return self._reject(ctx, "add", "", type, source, "rejected_env")
        # ④ provenance:add 无 target,跳过
        # ①⑤ 校验+写入:内容校验由 store.add 内部完成(唯一校验源),
        # 非法内容抛 ValueError→转 invalid,service 不再重复预校验。
        entry = MemoryEntry(
            type=type,
            key=key,
            content=content,
            tags=tags,
            importance=importance,
            source=source,
            source_session=ctx.memory_scope,
            pinned=pinned,
        )
        try:
            with self._service_write():
                stored = self._store.add(entry)
        except ValueError:
            return self._reject(ctx, "add", "", type, source, "invalid")
        # ⑥⑦ flush→失效 ⑧ 审计
        await self._finalize(ctx, "add", stored.id, type, source, "", True)
        return WriteResult(ok=True, entry=stored)

    async def replace(
        self,
        ctx: ActorContext,
        entry_id: str,
        *,
        content: str,
        source: str,
        tags: list[str] | None = None,
        override: bool = False,
    ) -> WriteResult:
        """替换既有条目内容,走完整八步写序(含 provenance 守卫)。

        override=True 时跳过第④步 provenance(admin 通道显式越权);scope/ENV
        门禁与失效/审计仍照常执行。tags 非 None 时一并更新条目标签。
        """
        target = self._store.get(entry_id)
        if target is None:
            return self._reject(ctx, "replace", entry_id, None, source, "invalid")
        # ② scope 门禁
        scope_rej = self._scope_gate(target.type, ctx)
        if scope_rej:
            return self._reject(ctx, "replace", entry_id, target.type, source, scope_rej)
        # ③ ENV 门禁
        if self._env_denied(ctx.actor, target.type, target.tags):
            return self._reject(ctx, "replace", entry_id, target.type, source, "rejected_env")
        # ④ provenance:低于目标优先级则拒。R4:与 add 路径(_merge_locked→
        # _spawn_blocked_contradiction)对齐,拒前先把被拒内容记为可裁决的
        # contradiction(落真实 pending 条目,不进召回),交 reflection 消费;
        # 被拒仍不改 active(target 不动)。override 显式越权跳过。
        if not override and not provenance_guard(source, target):
            blocked = MemoryEntry(
                type=target.type,
                key=target.key,
                content=content,
                tags=list(tags or []),
                source_session=ctx.memory_scope,
                source=source,
            )
            # landing(同步写盘)入 _service_write 锁,SQL 行 await 紧随其后落盘——
            # 不再 fire-and-forget,消除"有 pending 条目、无 contradiction 行"的孤儿窗口。
            # _service_write() 是同步上下文管理器包不住 await,故 landing 在锁内、
            # SQL await 在锁外。
            land = getattr(self._store, "_land_blocked_entry", None)
            record = getattr(self._store, "record_blocked_contradiction", None)
            if land is not None and record is not None:
                try:
                    with self._service_write():
                        landed = land(target, blocked)
                    await record(target, landed)
                except Exception as e:
                    logger.warning("replace 被拒写 contradiction 失败: {}", e)
            else:
                # 旧 store/测试桩兜底:退回同步 spawn 路径。
                spawn = getattr(self._store, "_spawn_blocked_contradiction", None)
                if spawn is not None:
                    try:
                        spawn(target, blocked)
                    except Exception as e:
                        logger.warning("replace 被拒写 contradiction 失败: {}", e)
            return self._reject(ctx, "replace", entry_id, target.type, source, "rejected_provenance")
        # ①⑤ 校验+写入:内容校验由 store.update 内部完成(唯一校验源),
        # 非法内容抛 ValueError→转 invalid。
        try:
            with self._service_write():
                updated = self._store.update(entry_id, content=content, tags=tags, source=source)
        except ValueError:
            return self._reject(ctx, "replace", entry_id, target.type, source, "invalid")
        if updated is None:
            return self._reject(ctx, "replace", entry_id, target.type, source, "invalid")
        await self._finalize(ctx, "replace", entry_id, target.type, source, "", True)
        return WriteResult(ok=True, entry=updated)

    async def remove(
        self, ctx: ActorContext, entry_id: str, *, override: bool = False
    ) -> WriteResult:
        """删除既有条目,走完整八步写序(含 provenance 守卫)。

        override=True 时跳过第④步 provenance(admin 通道显式越权)。
        """
        target = self._store.get(entry_id)
        if target is None:
            return self._reject(ctx, "remove", entry_id, None, "", "invalid")
        derived = _ACTOR_SOURCE.get(ctx.actor, "legacy")
        # ② scope 门禁
        scope_rej = self._scope_gate(target.type, ctx)
        if scope_rej:
            return self._reject(ctx, "remove", entry_id, target.type, derived, scope_rej)
        # ③ ENV 门禁
        if self._env_denied(ctx.actor, target.type, target.tags):
            return self._reject(ctx, "remove", entry_id, target.type, derived, "rejected_env")
        # ④ provenance;override 显式越权跳过。
        # 决策2:maintenance 是内部维护(归档/遗忘删除,非用户/模型行为),与它
        # set_tier/maintenance_update 的免检身份一致,跳过 provenance——否则
        # _ACTOR_SOURCE 把 maintenance 映射 legacy(rank0)会拦下任何有主条目。
        if not override and ctx.actor != "maintenance" and not provenance_guard(derived, target):
            return self._reject(ctx, "remove", entry_id, target.type, derived, "rejected_provenance")
        # ⑤ 写入
        with self._service_write():
            ok = self._store.delete(entry_id)
        if not ok:
            return self._reject(ctx, "remove", entry_id, target.type, derived, "invalid")
        await self._finalize(ctx, "remove", entry_id, target.type, derived, "", True)
        return WriteResult(ok=True, entry=target)

    async def promote(
        self,
        ctx: ActorContext,
        *,
        type: MemoryType,
        key: str,
        content: str,
        tags: list[str] | None = None,
        importance: float,
        source: str = "consolidated",
    ) -> WriteResult:
        """整合提升(consolidation)写入新条目,写序与 add 一致(无 target 跳过 provenance)。"""
        return await self.add(
            ctx,
            type=type,
            key=key,
            content=content,
            tags=tags,
            importance=importance,
            source=source,
        )

    # ── 精简写序:裁决者/内部维护(跳过 provenance 与 ENV 门禁) ──────────────

    async def maintenance_update(
        self,
        ctx: ActorContext,
        entry_id: str,
        *,
        tags: list[str] | None = None,
        content: str | None = None,
        source: str | None = None,
    ) -> WriteResult:
        """内部维护更新:跳过 provenance/ENV 门禁,仍失效+flush+审计。"""
        target = self._store.get(entry_id)
        if target is None:
            return self._reject(ctx, "maintenance_update", entry_id, None, source or "", "invalid")
        try:
            with self._service_write():
                updated = self._store.update(entry_id, content=content, tags=tags, source=source)
        except ValueError:
            return self._reject(ctx, "maintenance_update", entry_id, target.type, source or "", "invalid")
        if updated is None:
            return self._reject(ctx, "maintenance_update", entry_id, target.type, source or "", "invalid")
        await self._finalize(ctx, "maintenance_update", entry_id, target.type, source or "", "", True)
        return WriteResult(ok=True, entry=updated)

    async def mark_superseded(
        self, ctx: ActorContext, entry_id: str, superseded_by: str
    ) -> WriteResult:
        """标记条目被取代:裁决动作,精简写序。"""
        target = self._store.get(entry_id)
        if target is None:
            return self._reject(ctx, "mark_superseded", entry_id, None, "", "invalid")
        with self._service_write():
            ok = self._store.mark_superseded(entry_id, superseded_by)
        if not ok:
            return self._reject(ctx, "mark_superseded", entry_id, target.type, "", "invalid")
        await self._finalize(ctx, "mark_superseded", entry_id, target.type, "", "", True)
        return WriteResult(ok=True, entry=self._store.get(entry_id))

    async def set_tier(
        self, ctx: ActorContext, entry_id: str, tier: MemoryTier
    ) -> WriteResult:
        """调整条目 tier(如归档):内部维护,精简写序。"""
        target = self._store.get(entry_id)
        if target is None:
            return self._reject(ctx, "set_tier", entry_id, None, "", "invalid")
        with self._service_write():
            ok = self._store.set_tier(entry_id, tier)
        if not ok:
            return self._reject(ctx, "set_tier", entry_id, target.type, "", "invalid")
        await self._finalize(ctx, "set_tier", entry_id, target.type, "", "", True)
        return WriteResult(ok=True, entry=self._store.get(entry_id))

    # ── 门禁 ───────────────────────────────────────────────────────────────

    @staticmethod
    def _scope_gate(mem_type: MemoryType | None, ctx: ActorContext) -> str:
        """USER 写 memory_scope 空则拒 rejected_scope;ENVIRONMENT 允许空 scope。"""
        if mem_type == MemoryType.USER and not ctx.memory_scope:
            return "rejected_scope"
        return ""

    def _env_denied(self, actor: str, mem_type: MemoryType | None, tags: list[str]) -> bool:
        """actor∈{model,reviewer,consolidation} 且(type==ENVIRONMENT 或含 global tag)且 not allow_env_writes → 拒。"""
        if self._allow_env_writes:
            return False
        if actor not in _ENV_RESTRICTED_ACTORS:
            return False
        return mem_type == MemoryType.ENVIRONMENT or "global" in (tags or [])

    # ── 失效 + flush + 审计 ──────────────────────────────────────────────────

    async def _finalize(
        self,
        ctx: ActorContext,
        op: str,
        entry_id: str,
        mem_type: MemoryType | None,
        source: str,
        reason: str,
        ok: bool,
    ) -> None:
        """⑥⑦ 失效前先 flush,再 invalidate;⑧ 追加审计。"""
        # ⑦ flush 在失效前
        if self._flush_fn is not None:
            try:
                await self._flush_fn()
            except Exception as e:
                logger.warning("MemoryService flush failed: {}", e)
        # ⑥ 失效:USER→(scope, False);ENV/裁决→(scope, True)
        if self._invalidate_fn is not None:
            global_scope = mem_type != MemoryType.USER
            try:
                await self._invalidate_fn(ctx.memory_scope, global_scope)
            except Exception as e:
                logger.warning("MemoryService invalidate failed: {}", e)
        # ⑧ 审计
        self._append_audit(ctx, op, entry_id, mem_type, source, reason, ok)

    def _reject(
        self,
        ctx: ActorContext,
        op: str,
        entry_id: str,
        mem_type: MemoryType | None,
        source: str,
        reason: str,
    ) -> WriteResult:
        """被拒路径:只审计拒绝事实,绝不失效/写库/写 contradiction。"""
        self._append_audit(ctx, op, entry_id, mem_type, source, reason, False)
        return WriteResult(ok=False, reason=reason)

    def _append_audit(
        self,
        ctx: ActorContext,
        op: str,
        entry_id: str,
        mem_type: MemoryType | None,
        source: str,
        reason: str,
        ok: bool,
    ) -> None:
        """JSONL/UTF-8 追加、超限轮转,仿 tool registry 的 _append_audit。"""
        if self._audit_path is None:
            return
        entry: dict[str, Any] = {
            "actor": ctx.actor,
            "op": op,
            "entry_id": entry_id,
            "scope": ctx.memory_scope,
            "source": source,
            "reason": reason,
            "ok": ok,
            "ts": datetime.now(timezone.utc).isoformat(),
        }
        try:
            self._audit_path.parent.mkdir(parents=True, exist_ok=True)
            if (
                self._audit_path.exists()
                and self._audit_path.stat().st_size > _MAX_AUDIT_FILE_BYTES
            ):
                rotated = self._audit_path.with_name(
                    f"{self._audit_path.stem}-{datetime.now(timezone.utc).strftime('%Y%m%d%H%M%S')}.jsonl"
                )
                self._audit_path.replace(rotated)
            with self._audit_path.open("a", encoding="utf-8") as f:
                f.write(json.dumps(entry, ensure_ascii=False, default=str) + "\n")
        except Exception as e:
            logger.debug("Failed to append memory audit entry: {}", e)
