"""Chat attachment upload — accept a file for one-shot use in a single chat turn.

Unlike the knowledge upload (which ingests into the persistent knowledge index),
attachments land in the gateway media cache under an ``attachments/`` subdir and are
referenced by an opaque id in the subsequent WebSocket ``message`` frame. The agent
resolves the id back to the cached path and extracts/reads the file for that turn only.
The media cache LRU cleanup reclaims these files automatically.
"""

from __future__ import annotations

import mimetypes
import secrets
from pathlib import Path
from typing import TYPE_CHECKING

from aiohttp import web
from loguru import logger

if TYPE_CHECKING:
    from echo_agent.gateway.server import GatewayServer

# Attachments live in this subdir of the media cache. Both the upload endpoint and
# the WS message handler derive paths from here, and path resolution is constrained
# to this dir to prevent traversal via a crafted attachment id.
ATTACHMENTS_SUBDIR = "attachments"


def attachments_dir(server: GatewayServer) -> Path:
    return server.media_cache.cache_dir / ATTACHMENTS_SUBDIR


def resolve_attachment_path(server: GatewayServer, attachment_id: str) -> Path | None:
    """Map an opaque attachment id back to a cached file, rejecting traversal.

    Returns the path only when it stays inside the attachments dir and exists.
    """
    if not attachment_id:
        return None
    base = attachments_dir(server).resolve()
    # Only the basename is honoured, so ids like "../../etc/passwd" collapse to a
    # plain name and can never escape the attachments dir.
    candidate = (base / Path(attachment_id).name).resolve()
    try:
        candidate.relative_to(base)
    except ValueError:
        return None
    if not candidate.is_file():
        return None
    return candidate


class ChatAttachmentAPI:
    # Hard cap per file; large docs are better suited to the knowledge index.
    _MAX_BYTES = 50 * 1024 * 1024

    def __init__(self, server: GatewayServer):
        self._server = server

    def _guard(self, request: web.Request, action: str) -> web.Response | None:
        return self._server._require_api_token(request, action=action)

    async def upload(self, request: web.Request) -> web.Response:
        guard = self._guard(request, "chat_attachment_upload")
        if guard is not None:
            return guard

        reader = await request.multipart()
        if reader is None:
            return web.json_response({"error": "multipart form required"}, status=400)

        dest_dir = attachments_dir(self._server)
        dest_dir.mkdir(parents=True, exist_ok=True)

        while True:
            part = await reader.next()
            if part is None:
                break
            if part.name != "file":
                continue

            original_name = Path(part.filename or "unnamed").name
            ext = Path(original_name).suffix.lower()
            # Opaque id = random token + original extension. The extension is kept so
            # downstream media-kind / document-extract detection still works by suffix.
            attachment_id = f"{secrets.token_hex(16)}{ext}"
            dest = dest_dir / attachment_id

            size = 0
            try:
                with open(dest, "wb") as f:
                    while True:
                        chunk = await part.read_chunk(8192)
                        if not chunk:
                            break
                        size += len(chunk)
                        if size > self._MAX_BYTES:
                            f.close()
                            dest.unlink(missing_ok=True)
                            return web.json_response(
                                {"error": "file too large", "max_bytes": self._MAX_BYTES},
                                status=413,
                            )
                        f.write(chunk)
            except OSError as e:
                logger.warning("Chat attachment write failed: {}", e)
                dest.unlink(missing_ok=True)
                return web.json_response({"error": "write failed"}, status=500)

            mime_type = (
                part.headers.get("Content-Type")
                or mimetypes.guess_type(original_name)[0]
                or "application/octet-stream"
            )
            logger.debug("Chat attachment stored: {} → {}", original_name, attachment_id)
            return web.json_response(
                {
                    "id": attachment_id,
                    "name": original_name,
                    "mime_type": mime_type,
                    "size": size,
                }
            )

        return web.json_response({"error": "no file field"}, status=400)
