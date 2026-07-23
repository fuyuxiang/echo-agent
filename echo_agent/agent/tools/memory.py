"""Agent-facing memory tool — add, replace, remove, search, list memories.

Gives the LLM direct control over what to remember about the user,
project, and environment across sessions.
"""

from __future__ import annotations

from typing import Any

from echo_agent.agent.tools.base import Tool, ToolExecutionContext, ToolResult
from echo_agent.memory.eligibility import Audience
from echo_agent.memory.store import MemoryEntry, MemoryType
from echo_agent.memory.service import ActorContext, MemoryService


class MemoryTool(Tool):
    name = "memory"
    description = (
        "Manage persistent memory across sessions. Actions: "
        "add (save a new memory), replace (update existing by key or substring match), "
        "remove (delete by key or substring), search (find relevant memories), "
        "list (show all memories of a type). "
        "list_contradictions (show unresolved memory conflicts for review), "
        "resolve_contradiction (pick winner_id to supersede the loser)."
    )
    parameters = {
        "type": "object",
        "properties": {
            "action": {
                "type": "string",
                "enum": ["add", "replace", "remove", "search", "list",
                         "list_contradictions", "resolve_contradiction"],
                "description": "The operation to perform",
            },
            "target": {
                "type": "string",
                "enum": ["user", "environment"],
                "description": "Memory type: 'user' for preferences/habits, 'environment' for project/tool facts",
            },
            "key": {
                "type": "string",
                "description": "Short label for the memory entry (for add/replace)",
            },
            "content": {
                "type": "string",
                "description": "Memory content (for add/replace)",
            },
            "old_text": {
                "type": "string",
                "description": "Substring to find the entry to replace/remove",
            },
            "query": {
                "type": "string",
                "description": "Search query (for search action)",
            },
            "tags": {
                "type": "string",
                "description": "Comma-separated tags (for add)",
            },
            "importance": {
                "type": "number",
                "description": "Importance score 0.0-1.0 (default 0.5)",
            },
            "pinned": {
                "type": "boolean",
                "description": (
                    "Pin this fact into the always-on core so it is present in "
                    "context on EVERY turn regardless of the current question "
                    "(for add). Reserve for must-never-forget facts (e.g. a hard "
                    "constraint, an allergy). Unpinned facts still surface via "
                    "recall when relevant. Defaults to false."
                ),
            },
            "source": {
                "type": "string",
                "enum": ["user_stated", "model_inferred"],
                "description": (
                    "Provenance of this memory (for add/replace). Use 'user_stated' ONLY "
                    "when the user explicitly asked to remember this or directly stated "
                    "the fact; otherwise omit (defaults to 'model_inferred')."
                ),
            },
            "contradiction_id": {
                "type": "string",
                "description": "Contradiction id (for resolve_contradiction)",
            },
            "winner_id": {
                "type": "string",
                "description": "Memory id that wins (for resolve_contradiction)",
            },
        },
        "required": ["action"],
    }

    def __init__(
        self,
        service: MemoryService,
        contradiction_detector: Any = None,
    ):
        # 统一写入口:provenance/ENV 门禁/失效/审计全部收敛在 service 的八步写序。
        # 工具只负责参数解析、读操作(find/search/list)与 WriteResult→ToolResult 映射。
        self._service = service
        self._store = service.store
        self._contradiction_detector = contradiction_detector

    def _resolve_entry(
        self,
        key: str,
        old_text: str,
        mem_type: MemoryType,
        session_key: str = "",
    ) -> tuple[MemoryEntry | None, str | None]:
        if key:
            entry = self._store.find_by_key(key, mem_type, session_key=session_key)
            if entry:
                return entry, None

        if old_text:
            matches = self._store.find_by_content_matches(
                old_text,
                mem_type=mem_type,
                limit=6,
                session_key=session_key,
            )
            if not matches:
                return None, None
            if len(matches) > 1:
                previews = ", ".join(entry.key or entry.content[:30] for entry in matches[:5])
                return None, (
                    f"Multiple matching memories found for old_text='{old_text}'. "
                    f"Be more specific. Matches: {previews}"
                )
            return matches[0], None

        return None, None

    async def execute(self, params: dict[str, Any], ctx: ToolExecutionContext | None = None) -> ToolResult:
        action = params.get("action", "")
        target = params.get("target", "user")
        mem_type = MemoryType.USER if target == "user" else MemoryType.ENVIRONMENT
        # 记忆作用域用 owner-aware 的 memory_scope(跨通道归一/群聊隔离都在其中);
        # 回退 session_key 兼容未经 loop 冻结构造的 ctx。变量名沿用 session_key
        # 因它直接喂给 store 的同名可见性参数。
        session_key = (ctx.memory_scope or ctx.session_key) if ctx else ""

        # ENV/global 门禁、写后失效、审计已全部收敛到 MemoryService 八步写序,
        # 工具入口不再重复实现。model actor 的 ActorContext 承载 scope 供 service 门禁。
        actor_ctx = ActorContext(
            actor="model",
            session_key=ctx.session_key if ctx else "",
            memory_scope=session_key,
        )

        if action == "add":
            return await self._add(params, mem_type, session_key, actor_ctx)
        elif action == "replace":
            return await self._replace(params, mem_type, session_key, actor_ctx)
        elif action == "remove":
            return await self._remove(params, mem_type, session_key, actor_ctx)
        elif action == "search":
            return self._search(params, mem_type, session_key)
        elif action == "list":
            return self._list(mem_type, session_key)
        elif action == "list_contradictions":
            return await self._list_contradictions(session_key)
        elif action == "resolve_contradiction":
            return await self._resolve_contradiction(params, session_key)
        else:
            return ToolResult(success=False, error=f"Unknown action '{action}'")

    async def _add(
        self, params: dict[str, Any], mem_type: MemoryType, session_key: str, actor_ctx: ActorContext
    ) -> ToolResult:
        key = params.get("key", "")
        content = params.get("content", "")
        if not key or not content:
            return ToolResult(success=False, error="key and content are required for add")
        tags_str = params.get("tags", "")
        tags = [t.strip() for t in tags_str.split(",") if t.strip()] if tags_str else []
        try:
            importance = min(1.0, max(0.0, float(params.get("importance", 0.5))))
        except (TypeError, ValueError):
            importance = 0.5
        source = params.get("source", "")
        if source not in ("user_stated", "model_inferred"):
            source = "model_inferred"
        pinned = bool(params.get("pinned", False))

        res = await self._service.add(
            actor_ctx,
            type=mem_type,
            key=key,
            content=content,
            tags=tags,
            importance=importance,
            source=source,
            pinned=pinned,
        )
        if not res.ok:
            return self._map_reject(res, "add", key)
        stored = res.entry
        if stored is not None and stored.content != content:
            # 决策1(R1 保持既有行为):store 内部 _merge_locked 对同 key 冲突
            # 保住高优先级旧内容并打 suspected_conflict——不谎称已写入新内容。
            return ToolResult(
                success=True,
                output=(
                    f"Kept existing entry (higher provenance): [{stored.type.value}] "
                    f"{stored.key} — conflict flagged for review"
                ),
            )
        return ToolResult(success=True, output=f"Memory saved: [{stored.type.value}] {stored.key}")

    async def _replace(
        self, params: dict[str, Any], mem_type: MemoryType, session_key: str, actor_ctx: ActorContext
    ) -> ToolResult:
        key = params.get("key", "")
        old_text = params.get("old_text", "")
        content = params.get("content", "")
        if not content:
            return ToolResult(success=False, error="content is required for replace")
        source = params.get("source", "")
        if source not in ("user_stated", "model_inferred"):
            source = "model_inferred"

        entry, resolve_error = self._resolve_entry(key, old_text, mem_type, session_key)
        if resolve_error:
            return ToolResult(success=False, error=resolve_error)
        if not entry:
            return ToolResult(success=False, error=f"No matching memory found for key='{key}' old_text='{old_text}'")

        # provenance 守卫、ENV/scope 门禁、失效、审计全在 service.replace 内。
        res = await self._service.replace(actor_ctx, entry.id, content=content, source=source)
        if not res.ok:
            return self._map_reject(res, "replace", entry.key)
        return ToolResult(success=True, output=f"Memory updated: [{entry.type.value}] {entry.key}")

    async def _remove(
        self, params: dict[str, Any], mem_type: MemoryType, session_key: str, actor_ctx: ActorContext
    ) -> ToolResult:
        key = params.get("key", "")
        old_text = params.get("old_text", "")

        entry, resolve_error = self._resolve_entry(key, old_text, mem_type, session_key)
        if resolve_error:
            return ToolResult(success=False, error=resolve_error)
        if not entry:
            return ToolResult(success=False, error=f"No matching memory found for key='{key}' old_text='{old_text}'")

        # provenance 守卫(据 model actor 的派生来源)、门禁、失效、审计全在 service.remove 内。
        res = await self._service.remove(actor_ctx, entry.id)
        if not res.ok:
            return self._map_reject(res, "remove", entry.key)
        return ToolResult(success=True, output=f"Memory removed: [{entry.type.value}] {entry.key}")

    @staticmethod
    def _map_reject(res, op: str, key: str) -> ToolResult:
        """WriteResult 拒绝原因 → ToolResult 错误文案。"""
        reason = res.reason
        if reason == "rejected_env":
            return ToolResult(
                success=False,
                error="writing ENVIRONMENT or global-tagged memory is disabled "
                      "(set memory.allow_model_environment_writes to enable)",
            )
        if reason == "rejected_provenance":
            verb = {"replace": "overwrite", "remove": "remove"}.get(op, op)
            return ToolResult(
                success=False,
                error=f"Cannot {verb} higher-provenance entry: {key}",
            )
        if reason == "rejected_scope":
            return ToolResult(
                success=False,
                error=f"Cannot write memory without a resolved scope: {key}",
            )
        # invalid / 其他:内容校验失败或目标缺失。
        return ToolResult(success=False, error=f"Memory {op} failed for '{key}' ({reason})")

    def _search(self, params: dict[str, Any], mem_type: MemoryType, session_key: str) -> ToolResult:
        query = params.get("query", "")
        if not query:
            return ToolResult(success=False, error="query is required for search")

        results = self._store.search_scored(
            query, mem_type, limit=10, session_key=session_key, audience=Audience.TOOL,
        )
        if not results:
            return ToolResult(success=True, output="No matching memories found.")

        self._store.reinforce([entry.id for entry, _ in results])

        lines = []
        for entry, score in results:
            tags = f" [{', '.join(entry.tags)}]" if entry.tags else ""
            lines.append(f"- [{entry.type.value}] **{entry.key}**{tags} (score={score:.2f}): {entry.content}")
        return ToolResult(success=True, output="\n".join(lines))

    def _list(self, mem_type: MemoryType, session_key: str) -> ToolResult:
        entries = self._store.list_all(mem_type, session_key=session_key, audience=Audience.TOOL)
        if not entries:
            return ToolResult(success=True, output=f"No {mem_type.value} memories found.")

        lines = []
        for e in entries[:50]:
            tags = f" [{', '.join(e.tags)}]" if e.tags else ""
            lines.append(f"- **{e.key}**{tags}: {e.content}")
        total = len(entries)
        if total > 50:
            lines.append(f"... and {total - 50} more")
        return ToolResult(success=True, output="\n".join(lines))

    async def _list_contradictions(self, memory_scope: str = "") -> ToolResult:
        if self._contradiction_detector is None:
            return ToolResult(success=False, error="Contradiction detection is disabled.")
        # 空 scope 不能写(见 _scope_gate 的 rejected_scope),同理不能看/裁决矛盾。
        if not memory_scope:
            return ToolResult(success=False, error="no memory scope in context")
        items = await self._contradiction_detector.get_unresolved(
            limit=20, memory_scope=memory_scope
        )
        if not items:
            return ToolResult(success=True, output="No unresolved contradictions.")
        lines = [
            f"- {c.id}: {c.description} (a={c.memory_id_a}, b={c.memory_id_b})"
            for c in items
        ]
        return ToolResult(success=True, output="\n".join(lines))

    async def _resolve_contradiction(self, params: dict[str, Any], session_key: str = "") -> ToolResult:
        if self._contradiction_detector is None:
            return ToolResult(success=False, error="Contradiction detection is disabled.")
        # 空 scope 直接拒绝(与 _scope_gate 的 rejected_scope 一致:不能写就不能裁决)。
        if not session_key:
            return ToolResult(success=False, error="no memory scope in context")
        cid = params.get("contradiction_id", "")
        winner_id = params.get("winner_id", "")
        if not cid or not winner_id:
            return ToolResult(success=False, error="contradiction_id and winner_id are required")
        unresolved = {
            c.id: c
            for c in await self._contradiction_detector.get_unresolved(
                limit=100, memory_scope=session_key
            )
        }
        c = unresolved.get(cid)
        if c is None:
            return ToolResult(success=False, error=f"No unresolved contradiction '{cid}'")
        # 边界鉴权:两端条目必须都对调用方 scope 可见才允许裁决。不能依赖
        # detector.resolve() 内部的 ActorContext——那个 ctx 用的是败者自己的 scope
        # (contradiction.py 构造 loser scope),等于自我放行,鉴权必须在工具边界完成。
        store = getattr(self._contradiction_detector, "_store", None)
        if store is not None:
            a = store.get(c.memory_id_a)
            b = store.get(c.memory_id_b)
            if (
                a is None or b is None
                or not store.is_visible_in_session(a, session_key)
                or not store.is_visible_in_session(b, session_key)
            ):
                return ToolResult(success=False, error="contradiction not in your scope")
        if winner_id not in (c.memory_id_a, c.memory_id_b):
            return ToolResult(success=False, error="winner_id must be memory_id_a or memory_id_b")
        resolution = "a_wins" if winner_id == c.memory_id_a else "b_wins"
        ok = await self._contradiction_detector.resolve(cid, resolution, winner_id=winner_id)
        if not ok:
            return ToolResult(
                success=False,
                error="resolution failed, contradiction stays open — retry later",
            )
        # 装配 service 的 detector,其 resolve 已走 detector→service.mark_superseded→
        # _finalize 完成失效(八步写序内),工具不得再显式 invalidate 二次失效。
        # 仅当 detector 未装配 service(裸 store 兜底路径)时,mark_superseded 直连
        # store 不触发失效,才由工具补一次全局失效——否则冻结快照/预取跨轮继续
        # 注入已被取代的败者条目。裁决全局可见,故兜底失效用全局。
        if getattr(self._contradiction_detector, "_service", None) is None:
            await self._service.invalidate(session_key, global_scope=True)
        return ToolResult(success=True, output=f"Resolved {cid}: {resolution}")
