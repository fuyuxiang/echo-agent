from __future__ import annotations

import os
import tempfile
from pathlib import Path
from typing import TYPE_CHECKING, Any

from aiohttp import web

from echo_agent.gateway.jobs import AsyncJobRegistry

_MAX_UPLOAD_FILE_BYTES = 50 * 1024 * 1024
_MAX_UPLOAD_BATCH_BYTES = 200 * 1024 * 1024

if TYPE_CHECKING:
    from echo_agent.gateway.server import GatewayServer


def _safe_relative_dest(docs_dir: Path, relpath: str) -> Path | None:
    """Resolve relpath under docs_dir, rejecting absolute paths and traversal.

    Resolve then verify the result stays within docs_dir. The ``+ os.sep``
    boundary on the prefix check prevents a sibling like ``docs-evil`` from
    being accepted under ``docs``. Returns None when the path escapes the
    root (or equals the root itself). Shared by upload and delete_document.
    """
    # Browsers normally submit POSIX separators even on Windows, but API
    # clients need the same traversal guarantees for backslash paths too.
    normalized = relpath.replace("\\", "/")
    if "\x00" in normalized:
        return None
    candidate = Path(normalized)
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
        registry = getattr(server, "_knowledge_jobs", None)
        if registry is None:
            registry = AsyncJobRegistry(
                event_sink=server.dashboard_ws.broadcast,
                event_type="knowledge_job_updated",
            )
            server._knowledge_jobs = registry
        self._jobs: AsyncJobRegistry = registry
        lock = getattr(server, "_knowledge_job_lock", None)
        if lock is None:
            import asyncio

            lock = asyncio.Lock()
            server._knowledge_job_lock = lock
        self._job_lock = lock

    def _index(self):
        return self._server._agent_loop.knowledge

    def _guard(self, request: web.Request, action: str) -> web.Response | None:
        return self._server._require_api_token(request, action=action)

    def _admin_guard(self, request: web.Request, action: str) -> web.Response | None:
        return self._server._require_admin_token(request, action=action)

    @staticmethod
    def _background(request: web.Request) -> bool:
        return request.query.get("background", "").lower() in {"1", "true", "yes"}

    async def _serialized_rebuild(self, index) -> dict[str, Any]:
        async with self._job_lock:
            return await index.rebuild_async()

    def _start_rebuild(self, index, action: str, **metadata: Any) -> dict[str, Any]:
        return self._jobs.start(
            action,
            lambda: self._serialized_rebuild(index),
            metadata=metadata,
        )

    async def get_status(self, request: web.Request) -> web.Response:
        guard = self._guard(request, "knowledge_status")
        if guard is not None:
            return guard

        index = self._index()
        if index is None:
            return web.json_response({"error": "knowledge index not configured"}, status=404)
        return web.json_response(index.status())

    async def rebuild(self, request: web.Request) -> web.Response:
        # A rebuild re-reads every document and recomputes embeddings — expensive,
        # repeatable, and it rewrites the index other callers are querying. It sits
        # with upload/delete (already admin-guarded) rather than with the read
        # endpoints, and being admin-guarded also blocks a cross-site POST from
        # kicking one off on a localhost gateway.
        guard = self._admin_guard(request, "knowledge_rebuild")
        if guard is not None:
            return guard

        index = self._index()
        if index is None:
            return web.json_response({"error": "knowledge index not configured"}, status=404)

        if self._background(request):
            job = self._start_rebuild(index, "rebuild")
            return web.json_response({"job": job}, status=202)
        result = await self._serialized_rebuild(index)
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

        uploaded: list[str] = []
        rejected: list[str] = []
        allowed = getattr(index, "allowed_extensions", None)
        if not isinstance(allowed, (set, frozenset, list, tuple)):
            allowed = None
        total_bytes = 0
        # No user-visible document is mutated until every part has passed path,
        # type, and size validation. TemporaryDirectory also guarantees cleanup
        # on malformed multipart streams and client disconnects.
        with tempfile.TemporaryDirectory(prefix=".upload-", dir=docs_dir) as temp_name:
            staging_dir = Path(temp_name)
            staged: dict[str, Path] = {}
            while True:
                part = await reader.next()
                if part is None:
                    break
                if part.name != "file":
                    await part.release()
                    continue
                raw_name = part.filename or "unnamed.txt"
                normalized_name = raw_name.replace("\\", "/")
                if allowed:
                    from pathlib import PurePosixPath

                    ext = PurePosixPath(normalized_name).suffix.lower()
                    if ext not in allowed:
                        rejected.append(raw_name)
                        await part.release()
                        continue
                dest = _safe_relative_dest(docs_dir, normalized_name)
                if dest is None:
                    return web.json_response({"error": f"invalid path: {raw_name}"}, status=400)
                relative = str(dest.relative_to(docs_dir.resolve()))
                stage = staging_dir / relative
                stage.parent.mkdir(parents=True, exist_ok=True)
                file_bytes = 0
                with open(stage, "wb") as file_handle:
                    while True:
                        chunk = await part.read_chunk(64 * 1024)
                        if not chunk:
                            break
                        file_bytes += len(chunk)
                        total_bytes += len(chunk)
                        if file_bytes > _MAX_UPLOAD_FILE_BYTES:
                            return web.json_response(
                                {"error": f"file too large: {raw_name}", "max_bytes": _MAX_UPLOAD_FILE_BYTES},
                                status=413,
                            )
                        if total_bytes > _MAX_UPLOAD_BATCH_BYTES:
                            return web.json_response(
                                {"error": "upload batch too large", "max_bytes": _MAX_UPLOAD_BATCH_BYTES},
                                status=413,
                            )
                        file_handle.write(chunk)
                staged[relative] = stage

            if not staged:
                if rejected:
                    return web.json_response(
                        {
                            "error": f"unsupported file type(s): {', '.join(rejected)}. "
                            f"Allowed: {', '.join(sorted(allowed))}",
                            "rejected": rejected,
                        },
                        status=400,
                    )
                return web.json_response({"error": "no files uploaded"}, status=400)

            # os.replace makes each committed document atomic for concurrent
            # readers; all validation above happens before the first commit.
            for relative, stage in staged.items():
                dest = _safe_relative_dest(docs_dir, relative)
                if dest is None:  # pragma: no cover - derived from validated path
                    return web.json_response({"error": f"invalid path: {relative}"}, status=400)
                dest.parent.mkdir(parents=True, exist_ok=True)
                os.replace(stage, dest)
                uploaded.append(relative)

        if not uploaded:
            return web.json_response({"error": "no files uploaded"}, status=400)

        if self._background(request):
            job = self._start_rebuild(index, "upload", documents=uploaded)
            resp: dict = {"uploaded": uploaded, "job": job}
            if rejected:
                resp["rejected"] = rejected
            return web.json_response(resp, status=202)

        result = await self._serialized_rebuild(index)
        resp: dict = {"uploaded": uploaded, "index": result}
        if rejected:
            resp["rejected"] = rejected
        return web.json_response(resp)

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
                documents.append(
                    {
                        "path": str(f.relative_to(docs_dir)),
                        "size": stat.st_size,
                        "modified": stat.st_mtime,
                    }
                )

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
        if self._background(request):
            job = self._start_rebuild(index, "delete", document=rel_path)
            return web.json_response({"status": "accepted", "job": job}, status=202)
        result = await self._serialized_rebuild(index)
        return web.json_response({"status": "deleted", "index": result})

    async def list_jobs(self, request: web.Request) -> web.Response:
        guard = self._guard(request, "knowledge_jobs")
        if guard is not None:
            return guard
        try:
            limit = int(request.query.get("limit", "20"))
        except (TypeError, ValueError):
            return web.json_response({"error": "invalid limit parameter"}, status=400)
        return web.json_response({"jobs": self._jobs.list(limit=limit)})

    async def get_job(self, request: web.Request) -> web.Response:
        guard = self._guard(request, "knowledge_job")
        if guard is not None:
            return guard
        job = self._jobs.get(request.match_info["id"])
        if job is None:
            return web.json_response({"error": "not found"}, status=404)
        return web.json_response({"job": job})

    async def cancel_job(self, request: web.Request) -> web.Response:
        guard = self._admin_guard(request, "knowledge_job_cancel")
        if guard is not None:
            return guard
        if not await self._jobs.cancel(request.match_info["id"]):
            return web.json_response({"error": "job is not cancellable"}, status=409)
        return web.json_response({"status": "cancelled"})
