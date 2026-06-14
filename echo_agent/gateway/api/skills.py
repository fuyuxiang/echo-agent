from __future__ import annotations

from typing import TYPE_CHECKING

from aiohttp import web

if TYPE_CHECKING:
    from echo_agent.gateway.server import GatewayServer


class SkillsAPI:
    def __init__(self, server: GatewayServer):
        self._server = server

    def _store(self):
        return self._server._agent_loop.skill_store

    def _guard(self, request: web.Request, action: str) -> web.Response | None:
        return self._server._require_api_token(request, action=action)

    async def list_skills(self, request: web.Request) -> web.Response:
        guard = self._guard(request, "skills_list")
        if guard:
            return guard

        store = self._store()
        skills = store.list_all()
        return web.json_response({
            "skills": [s.to_dict() for s in skills],
        })

    async def get_skill(self, request: web.Request) -> web.Response:
        guard = self._guard(request, "skills_get")
        if guard:
            return guard

        name = request.match_info["name"]
        store = self._store()
        content = store.read_skill(name)
        if content is None:
            return web.json_response({"error": "not found"}, status=404)

        files = store.list_files(name)
        return web.json_response({
            "name": name,
            "content": content,
            "files": files,
        })

    async def toggle_skill(self, request: web.Request) -> web.Response:
        guard = self._guard(request, "skills_toggle")
        if guard:
            return guard

        name = request.match_info["name"]
        store = self._store()

        all_skills = store.list_all()
        is_currently_active = any(s.name == name for s in all_skills)

        if is_currently_active:
            store.persist_disable(name)
            return web.json_response({"name": name, "enabled": False})
        else:
            store.persist_enable(name)
            return web.json_response({"name": name, "enabled": True})
