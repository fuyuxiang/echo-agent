from __future__ import annotations

from typing import TYPE_CHECKING

from aiohttp import web

if TYPE_CHECKING:
    from echo_agent.gateway.server import GatewayServer


class SessionsAPI:
    def __init__(self, server: GatewayServer):
        self._server = server

    def _guard(self, request: web.Request, action: str) -> web.Response | None:
        return self._server._require_api_token(request, action=action)

    async def list_sessions(self, request: web.Request) -> web.Response:
        guard = self._guard(request, "sessions_list")
        if guard is not None:
            return guard

        channel = request.query.get("channel")
        # 用 async 版:同步 list_sessions 在运行的事件循环里只能看到内存缓存,
        # 且 await 一个同步返回的 list 会抛 TypeError。
        sessions = await self._server.session_manager.list_sessions_async()

        if channel:
            sessions = [s for s in sessions if s.get("key", "").startswith(channel)]

        return web.json_response({"sessions": sessions, "total": len(sessions)})

    async def get_history(self, request: web.Request) -> web.Response:
        guard = self._guard(request, "sessions_history")
        if guard is not None:
            return guard

        key = request.match_info["key"]
        try:
            limit = int(request.query.get("limit", "100"))
        except (ValueError, TypeError):
            return web.json_response({"error": "invalid limit parameter"}, status=400)

        # 只读取,不创建:此前用 get_or_create,查询一个不存在的 key 会真的建出一个
        # 空会话并写进 LRU 缓存(还可能连带驱逐、落盘另一个会话)——一个 GET 产生了
        # 持久化副作用,列表页因此会多出用户从未开启过的会话。
        session = await self._server.session_manager.get(key)
        if session is None:
            return web.json_response({"error": "not found"}, status=404)
        messages = session.get_history(max_messages=limit)

        return web.json_response({"messages": messages, "total": len(messages)})
