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

    def _list_all_with_status(self) -> list[dict]:
        """Return all skills (including disabled) with their enabled status."""
        store = self._store()
        results: list[dict] = []
        seen: set[str] = set()
        for root, _ in store._all_roots():
            if not root.exists():
                continue
            for skill_md in root.rglob("SKILL.md"):
                meta = store._read_meta(skill_md.parent)
                if meta and meta.name not in seen:
                    seen.add(meta.name)
                    d = meta.to_dict()
                    d["enabled"] = meta.name not in store._disabled
                    results.append(d)
        results.sort(key=lambda m: m["name"])
        return results

    async def list_skills(self, request: web.Request) -> web.Response:
        guard = self._guard(request, "skills_list")
        if guard is not None:
            return guard

        skills = self._list_all_with_status()
        return web.json_response({"skills": skills})

    async def get_skill(self, request: web.Request) -> web.Response:
        guard = self._guard(request, "skills_get")
        if guard is not None:
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
        if guard is not None:
            return guard

        name = request.match_info["name"]
        store = self._store()

        is_currently_disabled = name in store._disabled

        if is_currently_disabled:
            store.persist_enable(name)
            enabled = True
        else:
            store.persist_disable(name)
            enabled = False

        return web.json_response({
            "success": True,
            "skill": {"name": name, "enabled": enabled},
        })

    async def delete_skill(self, request: web.Request) -> web.Response:
        guard = self._guard(request, "skills_delete")
        if guard is not None:
            return guard

        name = request.match_info["name"]
        store = self._store()
        error = store.delete_skill(name)
        if error:
            return web.json_response({"error": error}, status=400)

        store._disabled.discard(name)
        store._persisted_disabled.discard(name)
        store._save_persisted_disabled()

        return web.json_response({"success": True})

    async def import_skill(self, request: web.Request) -> web.Response:
        guard = self._guard(request, "skills_import")
        if guard is not None:
            return guard

        try:
            body = await request.json()
        except Exception:
            return web.json_response({"error": "invalid JSON body"}, status=400)

        path = body.get("path")
        if not path:
            return web.json_response({"error": "path is required"}, status=400)

        from pathlib import Path as P
        source = P(path)
        if not source.exists() or not (source / "SKILL.md").exists():
            return web.json_response(
                {"error": f"no SKILL.md found at '{path}'"}, status=400
            )

        store = self._store()
        meta = store._read_meta(source)
        if not meta:
            return web.json_response(
                {"error": "failed to parse SKILL.md"}, status=400
            )

        import shutil
        target = store._user_dir / (meta.category or "general") / meta.name
        if target.exists():
            return web.json_response(
                {"error": f"skill '{meta.name}' already exists"}, status=409
            )
        target.mkdir(parents=True, exist_ok=True)
        shutil.copytree(source, target, dirs_exist_ok=True)

        d = meta.to_dict()
        d["enabled"] = False
        return web.json_response({"success": True, "skill": d})
