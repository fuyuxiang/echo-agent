"""Contradiction detection with versioned memory lattice.

Contradictions are not silently overwritten but stored as temporal edges,
supporting belief revision and history queries.
"""

from __future__ import annotations

import json
import re
import uuid
from datetime import datetime
from typing import TYPE_CHECKING, Any, Callable

from loguru import logger

from echo_agent.memory.types import Contradiction, MemoryEntry

if TYPE_CHECKING:
    from echo_agent.memory.vectors import VectorIndex
    from echo_agent.storage.backend import StorageBackend

_CONTRADICTION_TOOL = {
    "type": "function",
    "function": {
        "name": "check_contradiction",
        "description": "Determine whether two memory entries contradict each other.",
        "parameters": {
            "type": "object",
            "properties": {
                "is_contradictory": {
                    "type": "boolean",
                    "description": "True if the two memories contradict each other.",
                },
                "explanation": {
                    "type": "string",
                    "description": "Brief explanation of why they do or do not contradict.",
                },
            },
            "required": ["is_contradictory", "explanation"],
        },
    },
}


class ContradictionDetector:
    """Detects contradictions between memories using semantic similarity + LLM verification.

    Implements a versioned memory lattice: contradictions are not silently overwritten
    but stored as temporal edges, supporting belief revision and history queries.
    """

    SIMILARITY_THRESHOLD = 0.75
    STRONG_SIMILARITY_THRESHOLD = 0.85
    TEMPORAL_CONFLICT_DAYS = 1

    def __init__(
        self,
        storage: StorageBackend,
        vector_index: VectorIndex | None = None,
        store: Any = None,
        service: Any = None,
    ) -> None:
        self._storage = storage
        self._vector_index = vector_index
        # Authoritative MemoryStore — 矛盾镜像跟踪(unresolved 标记/清除)仍直接落
        # store,因为检索按 JSON 加载的条目过滤,而非镜像。
        self._store = store
        # MemoryService — supersede 标记(mark_superseded)改走 maintenance 通道,
        # 统一失效+审计;裁决是内部维护动作,跳 provenance/ENV 门禁。
        self._service = service

    # ------------------------------------------------------------------
    # Public API
    # ------------------------------------------------------------------

    MAX_LLM_CANDIDATES = 5

    async def check(
        self,
        new_entry: MemoryEntry,
        candidates: list[MemoryEntry],
        llm_call: Callable[..., Any] | None = None,
        embed_fn: Callable[..., Any] | None = None,
    ) -> list[Contradiction]:
        """Check new_entry against candidates for contradictions.

        Pre-filters candidates using heuristic (same key) and vector similarity
        before calling LLM, to avoid O(n) LLM calls.
        """
        contradictions: list[Contradiction] = []
        filtered = await self._pre_filter(new_entry, candidates, embed_fn)

        for candidate in filtered:
            if candidate.id == new_entry.id:
                continue
            result = await self._check_pair(new_entry, candidate, llm_call)
            if result is not None:
                contradictions.append(result)
        if contradictions:
            logger.info(
                "Detected {} contradiction(s) for memory {}",
                len(contradictions),
                new_entry.id,
            )
        return contradictions

    async def _pre_filter(
        self,
        new_entry: MemoryEntry,
        candidates: list[MemoryEntry],
        embed_fn: Callable[..., Any] | None = None,
    ) -> list[MemoryEntry]:
        """Pre-filter candidates to reduce LLM calls.

        Strategy:
        1. Always include same-key candidates (heuristic match)
        2. If vector index available, include high-similarity entries even with different keys
        3. Cap total at MAX_LLM_CANDIDATES
        """
        # 兜底排除 superseded:同 key 改口后的旧版本不应充当矛盾候选,否则会把
        # 新 active 版本判"矛盾"并误标 unresolved。整合器 Step 3 已在调用方过滤,
        # 此处双保险,防其它调用方(如未来直连 check 的路径)漏过滤。
        same_key = [
            c for c in candidates
            if c.key and c.key == new_entry.key and c.id != new_entry.id and not c.is_superseded
        ]
        same_key_ids = {c.id for c in same_key}

        vector_matches: list[MemoryEntry] = []
        if self._vector_index and embed_fn and new_entry.content:
            try:
                text = f"{new_entry.key} {new_entry.content}" if new_entry.key else new_entry.content
                embedding = await embed_fn(text)
                if embedding:
                    results = await self._vector_index.search(embedding, limit=self.MAX_LLM_CANDIDATES * 2)
                    candidate_map = {c.id: c for c in candidates}
                    for source_id, score in results:
                        if source_id in candidate_map and source_id not in same_key_ids:
                            if score >= self.STRONG_SIMILARITY_THRESHOLD:
                                vector_matches.append(candidate_map[source_id])
                            elif score >= self.SIMILARITY_THRESHOLD and candidate_map[source_id].key == new_entry.key:
                                vector_matches.append(candidate_map[source_id])
            except Exception as e:
                logger.debug("Vector pre-filter failed: {}", e)

        combined = same_key + vector_matches
        return combined[:self.MAX_LLM_CANDIDATES]

    async def store_contradiction(self, contradiction: Contradiction) -> None:
        """Persist contradiction to storage."""
        await self._storage.execute_sql(
            "INSERT OR REPLACE INTO memory_contradictions"
            "(id, memory_id_a, memory_id_b, description, resolution, resolved_at, created_at) "
            "VALUES (?, ?, ?, ?, ?, ?, ?)",
            (
                contradiction.id,
                contradiction.memory_id_a,
                contradiction.memory_id_b,
                contradiction.description,
                contradiction.resolution,
                contradiction.resolved_at,
                contradiction.created_at,
            ),
        )
        marker = getattr(self._store, "mark_contradiction_unresolved", None)
        if marker is not None:
            marker(contradiction.id, contradiction.memory_id_a, contradiction.memory_id_b)
        logger.debug("Stored contradiction {}", contradiction.id)

    async def resolve(
        self,
        contradiction_id: str,
        resolution: str,
        winner_id: str | None = None,
    ) -> bool:
        """Resolve a contradiction. resolution: 'a_wins', 'b_wins', 'merged', 'user_decided'.

        顺序:先 supersede 败者(事实源),成功才关 SQL 行 + 清镜像。此前顺序相反——
        先关行清镜像、后 supersede,supersede 失败时矛盾已从 unresolved 消失,不可重试,
        败者永久 active。改后 supersede 失败则整体不动、行保持 unresolved、可重试。
        返回 True 表示已裁决(或幂等已完成),False 表示未知/已裁决/supersede 失败。
        """
        rows = await self._storage.fetch_sql(
            "SELECT memory_id_a, memory_id_b FROM memory_contradictions "
            "WHERE id = ? AND resolution IS NULL",
            (contradiction_id,),
        )
        if not rows:
            return False  # 未知或已裁决

        # ① 先 supersede 败者(事实源),失败则整体不动、行保持 unresolved、可重试。
        if winner_id and resolution in ("a_wins", "b_wins"):
            row = rows[0]
            loser_id = (
                row["memory_id_b"] if winner_id == row["memory_id_a"] else row["memory_id_a"]
            )
            loser = self._store.get(loser_id) if self._store is not None else None
            if loser is not None and not loser.is_superseded:
                if self._service is not None:
                    # 裁决(mark_superseded)走 service maintenance 通道:统一失效+审计。
                    # 失效落在败者所属 scope;取不到条目时退回全局失效(裁决全局可见)。
                    from echo_agent.memory.service import ActorContext
                    scope = loser.source_session or ""
                    ctx = ActorContext(
                        actor="maintenance", session_key=scope, memory_scope=scope
                    )
                    res = await self._service.mark_superseded(ctx, loser_id, winner_id)
                    if not res.ok:
                        logger.warning(
                            "supersede loser {} failed, contradiction {} stays open",
                            loser_id, contradiction_id,
                        )
                        return False
                elif self._store is not None:
                    if not self._store.mark_superseded(loser_id, winner_id):
                        logger.warning(
                            "supersede loser {} failed (store), contradiction {} stays open",
                            loser_id, contradiction_id,
                        )
                        return False
                else:
                    # Mirror-only fallback — has no effect on retrieval, which
                    # reads the JSON store; kept for storage-only callers.
                    await self._storage.execute_sql(
                        "UPDATE memories SET superseded_by = ? WHERE id = ?",
                        (winner_id, loser_id),
                    )
            # loser 缺失或已 superseded → 视为已完成,继续关行(幂等,避免永久卡死的行)。

        # ② 后关 SQL 行 + 清镜像。
        now = datetime.now().isoformat()
        await self._storage.execute_sql(
            "UPDATE memory_contradictions SET resolution = ?, resolved_at = ? WHERE id = ?",
            (resolution, now, contradiction_id),
        )
        logger.info("Resolved contradiction {} as '{}'", contradiction_id, resolution)
        clearer = getattr(self._store, "clear_contradiction", None)
        if clearer is not None:
            clearer(contradiction_id)
        return True

    async def get_unresolved(
        self, limit: int = 10, memory_scope: str | None = None,
    ) -> list[Contradiction]:
        """Get unresolved contradictions.

        memory_scope=None → 全库语义,仅限内部维护调用方(启动时重建 unresolved 镜像)。
        memory_scope 给定 → 逐条解析两端 entry,两端都对该 scope 可见才返回,防跨 scope 泄露。
        """
        if memory_scope is None or self._store is None:
            rows = await self._storage.fetch_sql(
                "SELECT * FROM memory_contradictions WHERE resolution IS NULL "
                "ORDER BY created_at DESC LIMIT ?",
                (limit,),
            )
            return [Contradiction.from_dict(r) for r in rows]
        # 过滤模式下不带 SQL LIMIT:否则其他 scope 的行占满窗口会让本 scope 饥饿,
        # 过滤后再截 limit。
        rows = await self._storage.fetch_sql(
            "SELECT * FROM memory_contradictions WHERE resolution IS NULL "
            "ORDER BY created_at DESC",
        )
        out: list[Contradiction] = []
        for r in rows:
            c = Contradiction.from_dict(r)
            a = self._store.get(c.memory_id_a)
            b = self._store.get(c.memory_id_b)
            if a is None or b is None:
                continue
            if (
                self._store.is_visible_in_session(a, memory_scope)
                and self._store.is_visible_in_session(b, memory_scope)
            ):
                out.append(c)
            if len(out) >= limit:
                break
        return out

    async def get_history(self, memory_id: str) -> list[Contradiction]:
        """Get all contradictions involving a memory."""
        rows = await self._storage.fetch_sql(
            "SELECT * FROM memory_contradictions "
            "WHERE memory_id_a = ? OR memory_id_b = ? "
            "ORDER BY created_at DESC",
            (memory_id, memory_id),
        )
        return [Contradiction.from_dict(r) for r in rows]

    # supersede() 死方法已删除:曾是唯一 version+1 处但无存活调用者,version 递增
    # 现由 store.append_version 唯一接管;取代裁决走 resolve → mark_superseded。

    @staticmethod
    def _key_prefix(key: str) -> str:
        return key.split(":")[0] if ":" in key else key

    @staticmethod
    def _content_overlap(a: str, b: str) -> bool:
        """Cheap lexical relatedness check: shared word token or containment."""
        tokens_a = {t for t in re.findall(r"\w+", a.lower()) if len(t) > 1}
        tokens_b = {t for t in re.findall(r"\w+", b.lower()) if len(t) > 1}
        if tokens_a & tokens_b:
            return True
        a_s, b_s = a.strip().lower(), b.strip().lower()
        return bool(a_s and b_s and (a_s in b_s or b_s in a_s))

    def check_lightweight_sync(
        self,
        new_entry: MemoryEntry,
        candidates: list[MemoryEntry],
    ) -> list[Contradiction]:
        """Synchronous lightweight check (heuristic + temporal only, no LLM/vector).

        Candidates are matched by key *prefix* (e.g. ``pref:lang`` vs
        ``pref:editor`` share prefix ``pref``), not full key. Full-key matches are
        already handled deterministically by MemoryStore._merge_locked before this
        scan runs, so prefix matching is what lets this observe-only scan cover the
        remaining gap: semantically related entries under different full keys.
        """
        contradictions: list[Contradiction] = []
        new_prefix = self._key_prefix(new_entry.key) if new_entry.key else ""
        if not new_prefix:
            return contradictions
        same_prefix = [
            c for c in candidates
            if c.key and self._key_prefix(c.key) == new_prefix and c.id != new_entry.id
        ]

        for candidate in same_prefix[:self.MAX_LLM_CANDIDATES]:
            result = self._heuristic_check(new_entry, candidate)
            if result is None:
                result = self._temporal_conflict_check(new_entry, candidate)
            if result is not None:
                contradictions.append(result)
        return contradictions

    def _temporal_conflict_check(
        self, a: MemoryEntry, b: MemoryEntry
    ) -> Contradiction | None:
        """Detect temporal conflicts: same key prefix with different content over time."""
        if not a.key or not b.key:
            return None
        a_prefix = a.key.split(":")[0] if ":" in a.key else a.key
        b_prefix = b.key.split(":")[0] if ":" in b.key else b.key
        if a_prefix != b_prefix:
            return None
        if a.content.strip() == b.content.strip():
            return None
        # Prefix-only matches (different full keys) additionally require some
        # lexical relatedness — otherwise every pair in a namespace flags each
        # other (e.g. pref:lang="Python" vs pref:editor="vim"), and over time
        # the whole namespace drowns in suspected_conflict noise.
        if a.key != b.key and not self._content_overlap(a.content, b.content):
            return None

        a_time = a.updated_at or a.created_at
        b_time = b.updated_at or b.created_at
        if not a_time or not b_time:
            return None

        try:
            if isinstance(a_time, str):
                a_dt = datetime.fromisoformat(a_time)
            else:
                a_dt = a_time
            if isinstance(b_time, str):
                b_dt = datetime.fromisoformat(b_time)
            else:
                b_dt = b_time
            delta = abs((a_dt - b_dt).total_seconds())
            if delta >= self.TEMPORAL_CONFLICT_DAYS * 86400:
                newer = a if a_dt > b_dt else b
                older = b if newer is a else a
                return Contradiction(
                    id=uuid.uuid4().hex[:12],
                    memory_id_a=older.id,
                    memory_id_b=newer.id,
                    description=f"Temporal conflict on '{a_prefix}': content differs across time boundary.",
                )
        except (TypeError, ValueError):
            pass
        return None

    # ------------------------------------------------------------------
    # Internal helpers
    # ------------------------------------------------------------------

    async def _check_pair(
        self,
        new_entry: MemoryEntry,
        candidate: MemoryEntry,
        llm_call: Callable[..., Any] | None,
    ) -> Contradiction | None:
        if llm_call is not None:
            return await self._llm_check(new_entry, candidate, llm_call)
        return self._heuristic_check(new_entry, candidate)

    def _heuristic_check(
        self, a: MemoryEntry, b: MemoryEntry
    ) -> Contradiction | None:
        """Same key but different content implies contradiction."""
        if a.key and a.key == b.key and a.content.strip() != b.content.strip():
            return Contradiction(
                id=uuid.uuid4().hex[:12],
                memory_id_a=a.id,
                memory_id_b=b.id,
                description=f"Key '{a.key}' has conflicting content.",
            )
        return None

    async def _llm_check(
        self,
        a: MemoryEntry,
        b: MemoryEntry,
        llm_call: Callable[..., Any],
    ) -> Contradiction | None:
        messages = [
            {
                "role": "system",
                "content": (
                    "You are a contradiction detector. Determine whether the two "
                    "memory entries below contradict each other. Use the provided tool "
                    "to report your finding."
                ),
            },
            {
                "role": "user",
                "content": (
                    f"Memory A (key={a.key!r}):\n{a.content}\n\n"
                    f"Memory B (key={b.key!r}):\n{b.content}"
                ),
            },
        ]
        try:
            response = await llm_call(
                messages=messages,
                tools=[_CONTRADICTION_TOOL],
                tool_choice={"type": "function", "function": {"name": "check_contradiction"}},
            )
            args = response.tool_calls[0].arguments
            if isinstance(args, str):
                args = json.loads(args)
            if args.get("is_contradictory"):
                return Contradiction(
                    id=uuid.uuid4().hex[:12],
                    memory_id_a=a.id,
                    memory_id_b=b.id,
                    description=args.get("explanation", "LLM detected contradiction."),
                )
        except Exception:
            logger.opt(exception=True).warning(
                "LLM contradiction check failed for {} vs {}, falling back to heuristic",
                a.id,
                b.id,
            )
            return self._heuristic_check(a, b)
        return None
