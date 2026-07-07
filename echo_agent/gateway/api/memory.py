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

    def _guard(self, request: web.Request, action: str) -> web.Response | None:
        return self._server._require_api_token(request, action=action)

    async def list_entries(self, request: web.Request) -> web.Response:
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
        mt = MemoryType(mem_type) if mem_type else None
        entries = store.list_all(mem_type=mt, session_key=session_key or None)

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
        guard = self._guard(request, "memory_stats")
        if guard is not None:
            return guard

        store = self._store()
        entries = store.list_all()

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
        guard = self._guard(request, "memory_get")
        if guard is not None:
            return guard

        entry_id = request.match_info["id"]
        entry = self._store().get(entry_id)
        if not entry:
            return web.json_response({"error": "not found"}, status=404)
        return web.json_response(entry.to_dict())

    async def update_entry(self, request: web.Request) -> web.Response:
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
        result = self._store().update(entry_id, content=content, tags=tags)
        if not result:
            return web.json_response({"error": "not found"}, status=404)
        return web.json_response(result.to_dict())

    async def delete_entry(self, request: web.Request) -> web.Response:
        guard = self._guard(request, "memory_delete")
        if guard is not None:
            return guard

        entry_id = request.match_info["id"]
        ok = self._store().delete(entry_id)
        if not ok:
            return web.json_response({"error": "not found"}, status=404)
        return web.json_response({"status": "deleted"})

    async def search(self, request: web.Request) -> web.Response:
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
        mt = MemoryType(mem_type) if mem_type else None
        results = self._store().search_scored(query, mem_type=mt, limit=limit)

        return web.json_response({
            "results": [
                {"entry": entry.to_dict(), "score": score}
                for entry, score in results
            ],
        })
