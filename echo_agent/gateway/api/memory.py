from __future__ import annotations

import json
from typing import TYPE_CHECKING

from aiohttp import web

if TYPE_CHECKING:
    from echo_agent.gateway.server import GatewayServer


class MemoryAPI:
    def __init__(self, server: GatewayServer):
        self._server = server

    def _store(self):
        return self._server._agent_loop.memory

    def _memory_enabled(self) -> bool:
        # memory.enabled 总开关:关闭时整套 REST 记忆端点不可用,统一 409。
        loop = getattr(self._server, "_agent_loop", None)
        if loop is None:
            return False
        return bool(loop.config.memory.enabled)

    def _disabled_response(self) -> web.Response:
        return web.json_response({"error": "memory disabled"}, status=409)

    def _guard(self, request: web.Request, action: str) -> web.Response | None:
        return self._server._require_admin_token(request, action=action)

    async def list_entries(self, request: web.Request) -> web.Response:
        if not self._memory_enabled():
            return self._disabled_response()
        guard = self._guard(request, "memory_list")
        if guard is not None:
            return guard

        store = self._store()
        mem_type = request.query.get("type")
        tier = request.query.get("tier")
        session_key = request.query.get("session_key")
        try:
            offset = int(request.query.get("offset", "0"))
            limit = int(request.query.get("limit", "50"))
        except (ValueError, TypeError):
            return web.json_response({"error": "invalid offset/limit parameter"}, status=400)

        from echo_agent.memory.types import MemoryType
        from echo_agent.memory.eligibility import Audience
        mt = MemoryType(mem_type) if mem_type else None
        include_all = request.query.get("include_all") == "true"
        entries = store.list_all(
            mem_type=mt,
            session_key=session_key or None,
            audience=Audience.ADMIN if include_all else Audience.RETRIEVAL,
        )

        if tier:
            entries = [e for e in entries if e.tier.value == tier]

        total = len(entries)
        entries = entries[offset:offset + limit]

        return web.json_response({
            "entries": [e.to_dict() for e in entries],
            "total": total,
            "offset": offset,
            "limit": limit,
        })

    async def stats(self, request: web.Request) -> web.Response:
        if not self._memory_enabled():
            return self._disabled_response()
        guard = self._guard(request, "memory_stats")
        if guard is not None:
            return guard

        store = self._store()
        session_key = request.query.get("session_key")
        entries = store.list_all(session_key=session_key or None)

        by_tier: dict[str, int] = {}
        by_type: dict[str, int] = {}
        for e in entries:
            by_tier[e.tier.value] = by_tier.get(e.tier.value, 0) + 1
            by_type[e.type.value] = by_type.get(e.type.value, 0) + 1

        return web.json_response({
            "total": len(entries),
            "by_tier": by_tier,
            "by_type": by_type,
        })

    async def get_entry(self, request: web.Request) -> web.Response:
        if not self._memory_enabled():
            return self._disabled_response()
        guard = self._guard(request, "memory_get")
        if guard is not None:
            return guard

        entry_id = request.match_info["id"]
        entry = self._store().get(entry_id)
        if not entry:
            return web.json_response({"error": "not found"}, status=404)
        return web.json_response(entry.to_dict())

    async def update_entry(self, request: web.Request) -> web.Response:
        if not self._memory_enabled():
            return self._disabled_response()
        guard = self._guard(request, "memory_update")
        if guard is not None:
            return guard

        entry_id = request.match_info["id"]
        try:
            body = await request.json()
        except json.JSONDecodeError:
            return web.json_response({"error": "invalid JSON"}, status=400)

        content = body.get("content")
        tags = body.get("tags")

        # 写权守卫:admin 默认也受守卫(source_priority("admin")=0,故对任何有来源
        # 条目都被拦),仅 override=true 显式越权。被拒仅返回 403,不打 tag/写 contradiction。
        from echo_agent.memory.types import provenance_guard
        store = self._store()
        entry = store.get(entry_id)
        override = body.get("override") is True
        if entry and not override and not provenance_guard("admin", entry):
            return web.json_response(
                {"error": "cannot overwrite higher-provenance entry; pass override=true to force"},
                status=403,
            )

        result = store.update(entry_id, content=content, tags=tags)
        if not result:
            return web.json_response({"error": "not found"}, status=404)

        # 写后失效:先 flush 待入索引的向量,再失效缓存。顺序反了会有窗口内
        # 缓存已失效、检索命中却读到未 flush 的旧向量。ENVIRONMENT 全局失效,
        # 其余按其 source_session 作 scope 失效。
        from echo_agent.memory.types import MemoryType
        loop = self._server._agent_loop
        global_scope = result.type == MemoryType.ENVIRONMENT
        scope = "" if global_scope else result.source_session
        await store.flush_pending_embeds()
        await loop._invalidate_memory_caches(scope, global_scope)
        return web.json_response(result.to_dict())

    async def delete_entry(self, request: web.Request) -> web.Response:
        if not self._memory_enabled():
            return self._disabled_response()
        guard = self._guard(request, "memory_delete")
        if guard is not None:
            return guard

        entry_id = request.match_info["id"]

        # 删权守卫:先取目标条目再删;admin 默认受守卫,override=true(取自 query)才越权。
        from echo_agent.memory.types import provenance_guard
        store = self._store()
        entry = store.get(entry_id)
        override = request.query.get("override") == "true"
        if entry and not override and not provenance_guard("admin", entry):
            return web.json_response(
                {"error": "cannot delete higher-provenance entry; pass override=true to force"},
                status=403,
            )

        ok = store.delete(entry_id)
        if not ok:
            return web.json_response({"error": "not found"}, status=404)

        # 写后失效:先 flush 再失效缓存(见 update_entry)。scope 取删除前的 entry。
        from echo_agent.memory.types import MemoryType
        loop = self._server._agent_loop
        global_scope = bool(entry and entry.type == MemoryType.ENVIRONMENT)
        scope = "" if global_scope else (entry.source_session if entry else "")
        await store.flush_pending_embeds()
        await loop._invalidate_memory_caches(scope, global_scope)
        return web.json_response({"status": "deleted"})

    async def search(self, request: web.Request) -> web.Response:
        if not self._memory_enabled():
            return self._disabled_response()
        guard = self._guard(request, "memory_search")
        if guard is not None:
            return guard

        try:
            body = await request.json()
        except json.JSONDecodeError:
            return web.json_response({"error": "invalid JSON"}, status=400)

        query = body.get("query", "")
        if not query:
            return web.json_response({"error": "query required"}, status=400)

        mem_type = body.get("type")
        limit = body.get("limit", 10)

        from echo_agent.memory.types import MemoryType
        from echo_agent.memory.eligibility import Audience
        mt = MemoryType(mem_type) if mem_type else None
        session_key = body.get("session_key")
        # 无 session_key 即全量跨主体检索,是跨 scope 暴露口子;须显式 all_scopes=true 才放行。
        all_scopes = body.get("all_scopes") is True
        if not session_key and not all_scopes:
            return web.json_response(
                {"error": "session_key or all_scopes=true required"}, status=400
            )
        include_all = body.get("include_all") == "true"
        results = self._store().search_scored(
            query, mem_type=mt, limit=limit, session_key=session_key or None,
            audience=Audience.ADMIN if include_all else Audience.RETRIEVAL,
        )

        return web.json_response({
            "results": [
                {"entry": entry.to_dict(), "score": score}
                for entry, score in results
            ],
        })
