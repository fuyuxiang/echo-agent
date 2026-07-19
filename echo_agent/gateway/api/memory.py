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

    def _service(self):
        """REST 写端点(update/delete)统一走 MemoryService 的 admin 通道。

        Task 8 将全局收口为单例;本任务就近取 loop 上已构造的 service,缺失则
        用 loop 的 memory/失效/flush 就地构造一个最小 service。
        """
        loop = self._server._agent_loop
        service = getattr(loop, "_memory_service", None)
        if service is not None:
            return service
        from echo_agent.memory.service import MemoryService
        return MemoryService(
            loop.memory,
            invalidate_fn=loop._invalidate_memory_caches,
            flush_fn=getattr(loop.memory, "flush_pending_embeds", None),
            allow_env_writes=loop.config.memory.allow_model_environment_writes,
        )

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

        # 目标不存在直接 404(避免把 service 的 invalid 与 not-found 混为 400)。
        # scope 取目标条目的 source_session:USER 写据此过 scope 门禁并按 scope 失效。
        store = self._store()
        entry = store.get(entry_id)
        if entry is None:
            return web.json_response({"error": "not found"}, status=404)
        sk = entry.source_session

        from echo_agent.memory.service import ActorContext
        service = self._service()

        # admin 写权:默认受 provenance 守卫(source_priority("admin")=0,对任何有来源
        # 条目都被拦),仅 override=true 显式越权。provenance/门禁/写后 flush+失效/审计
        # 全在 service 八步写序内。tags-only 更新(无 content)走精简的 maintenance_update。
        actor = ActorContext(actor="admin", session_key=sk, memory_scope=sk)
        override = body.get("override") is True
        if content is not None:
            r = await service.replace(
                actor, entry_id, content=content, source="admin",
                tags=tags, override=override,
            )
        else:
            # tags-only 更新(无 content)同样受 admin provenance 守卫:maintenance_update
            # 走精简写序会跳过 provenance,故在此显式对目标条目施加 admin 守卫——
            # admin 派生源 priority 0,对任何有来源条目都被拦,仅 override=true 越权。
            from echo_agent.memory.types import provenance_guard
            if not override and not provenance_guard("admin", entry):
                return web.json_response(
                    {"error": "cannot overwrite higher-provenance entry; pass override=true to force"},
                    status=403,
                )
            r = await service.maintenance_update(actor, entry_id, tags=tags)

        if not r.ok:
            if r.reason == "rejected_provenance":
                return web.json_response(
                    {"error": "cannot overwrite higher-provenance entry; pass override=true to force"},
                    status=403,
                )
            if r.reason == "invalid":
                return web.json_response({"error": "invalid update"}, status=400)
            return web.json_response({"error": r.reason or "rejected"}, status=400)
        return web.json_response(r.entry.to_dict())

    async def delete_entry(self, request: web.Request) -> web.Response:
        if not self._memory_enabled():
            return self._disabled_response()
        guard = self._guard(request, "memory_delete")
        if guard is not None:
            return guard

        entry_id = request.match_info["id"]

        # 目标不存在直接 404;scope 取删除前 entry 的 source_session。
        store = self._store()
        entry = store.get(entry_id)
        if entry is None:
            return web.json_response({"error": "not found"}, status=404)
        sk = entry.source_session

        from echo_agent.memory.service import ActorContext
        service = self._service()

        # admin 删权:默认受守卫,override=true(取自 query)才越权。provenance/门禁/
        # 写后 flush+失效/审计全在 service.remove 八步写序内。
        override = request.query.get("override") == "true"
        r = await service.remove(
            ActorContext(actor="admin", session_key=sk, memory_scope=sk),
            entry_id, override=override,
        )
        if not r.ok:
            if r.reason == "rejected_provenance":
                return web.json_response(
                    {"error": "cannot delete higher-provenance entry; pass override=true to force"},
                    status=403,
                )
            if r.reason == "invalid":
                return web.json_response({"error": "not found"}, status=404)
            return web.json_response({"error": r.reason or "rejected"}, status=400)
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
