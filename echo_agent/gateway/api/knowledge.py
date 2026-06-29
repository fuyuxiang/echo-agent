from __future__ import annotations

import os
from pathlib import Path
from typing import TYPE_CHECKING

from aiohttp import web

if TYPE_CHECKING:
    from echo_agent.gateway.server import GatewayServer


def _safe_relative_dest(docs_dir: Path, relpath: str) -> Path | None:
    """Resolve relpath under docs_dir, rejecting absolute paths and traversal.

    Resolve then verify the result stays within docs_dir. The ``+ os.sep``
    boundary on the prefix check prevents a sibling like ``docs-evil`` from
    being accepted under ``docs``. Returns None when the path escapes the
    root (or equals the root itself). Shared by upload and delete_document.
    """
    candidate = Path(relpath)
    if candidate.is_absolute() or ".." in candidate.parts:
        return None
    base = docs_dir.resolve()
    target = (docs_dir / candidate).resolve()
    if not (str(target) == str(base) or str(target).startswith(str(base) + os.sep)):
        return None
    if target == base:
        return None
    return target


class KnowledgeAPI:
    def __init__(self, server: GatewayServer):
        self._server = server

    def _index(self):
        return self._server._agent_loop.knowledge

    def _guard(self, request: web.Request, action: str) -> web.Response | None:
        return self._server._require_api_token(request, action=action)

    def _admin_guard(self, request: web.Request, action: str) -> web.Response | None:
        return self._server._require_admin_token(request, action=action)

    async def get_status(self, request: web.Request) -> web.Response:
        guard = self._guard(request, "knowledge_status")
        if guard is not None:
            return guard

        index = self._index()
        if index is None:
            return web.json_response({"error": "knowledge index not configured"}, status=404)
        return web.json_response(index.status())

    async def rebuild(self, request: web.Request) -> web.Response:
        guard = self._guard(request, "knowledge_rebuild")
        if guard is not None:
            return guard

        index = self._index()
        if index is None:
            return web.json_response({"error": "knowledge index not configured"}, status=404)

        result = await index.rebuild_async()
        return web.json_response(result)

    async def upload(self, request: web.Request) -> web.Response:
        guard = self._admin_guard(request, "knowledge_upload")
        if guard is not None:
            return guard

        index = self._index()
        if index is None:
            return web.json_response({"error": "knowledge index not configured"}, status=404)

        reader = await request.multipart()
        if reader is None:
            return web.json_response({"error": "multipart form required"}, status=400)

        docs_dir = Path(index.status().get("docs_dir", ""))
        if not docs_dir.exists():
            docs_dir.mkdir(parents=True, exist_ok=True)

        uploaded = []
        while True:
            part = await reader.next()
            if part is None:
                break
            if part.name == "file":
                raw_name = part.filename or "unnamed.txt"
                # Use part.filename's relative path as-is for the target path;
                # _safe_relative_dest validates it stays within docs_dir,
                # rejecting traversal/absolute paths with a 400.
                relpath = raw_name
                dest = _safe_relative_dest(docs_dir, relpath)
                if dest is None:
                    return web.json_response(
                        {"error": f"invalid path: {raw_name}"}, status=400
                    )
                dest.parent.mkdir(parents=True, exist_ok=True)
                with open(dest, "wb") as f:
                    while True:
                        chunk = await part.read_chunk(8192)
                        if not chunk:
                            break
                        f.write(chunk)
                uploaded.append(str(dest.relative_to(docs_dir.resolve())))

        if not uploaded:
            return web.json_response({"error": "no files uploaded"}, status=400)

        result = await index.rebuild_async()
        return web.json_response({
            "uploaded": uploaded,
            "index": result,
        })

    async def list_documents(self, request: web.Request) -> web.Response:
        guard = self._guard(request, "knowledge_documents")
        if guard is not None:
            return guard

        index = self._index()
        if index is None:
            return web.json_response({"error": "knowledge index not configured"}, status=404)

        status = index.status()
        docs_dir = Path(status.get("docs_dir", ""))
        if not docs_dir.exists():
            return web.json_response({"documents": []})

        documents = []
        for f in sorted(docs_dir.rglob("*")):
            if f.is_file() and not f.name.startswith("."):
                stat = f.stat()
                documents.append({
                    "path": str(f.relative_to(docs_dir)),
                    "size": stat.st_size,
                    "modified": stat.st_mtime,
                })

        return web.json_response({"documents": documents})

    async def delete_document(self, request: web.Request) -> web.Response:
        guard = self._admin_guard(request, "knowledge_delete")
        if guard is not None:
            return guard

        index = self._index()
        if index is None:
            return web.json_response({"error": "knowledge index not configured"}, status=404)

        rel_path = request.match_info["path"]
        status = index.status()
        docs_dir = Path(status.get("docs_dir", ""))
        target = _safe_relative_dest(docs_dir, rel_path)

        if target is None:
            return web.json_response({"error": "invalid path"}, status=400)

        if not target.exists():
            return web.json_response({"error": "not found"}, status=404)

        os.remove(target)
        result = await index.rebuild_async()
        return web.json_response({"status": "deleted", "index": result})
