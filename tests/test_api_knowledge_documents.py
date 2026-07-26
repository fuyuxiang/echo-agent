# tests/test_api_knowledge_documents.py
"""Knowledge document endpoints: nested-path deletion and index status contract.

The delete route is registered with a tail match, so these tests exercise the
real registration (plus the SPA catch-all that sits after it) rather than a
hand-rolled single-route app — the regression they guard was purely a routing
artifact and is invisible when the handler is wired up directly.
"""
from __future__ import annotations

import os
from pathlib import Path
from unittest.mock import MagicMock

import pytest
from aiohttp import web
from aiohttp.test_utils import TestClient, TestServer

from echo_agent.gateway.api.knowledge import KnowledgeAPI
from echo_agent.knowledge import KnowledgeIndex


def _make_index(tmp_path: Path) -> KnowledgeIndex:
    docs = tmp_path / "docs"
    (docs / "sub").mkdir(parents=True)
    (docs / "root.md").write_text("# Root\n\nplain top level doc", encoding="utf-8")
    (docs / "sub" / "nested.md").write_text("# Nested\n\nlives in a subdir", encoding="utf-8")
    index = KnowledgeIndex(
        workspace=tmp_path,
        docs_dir="docs",
        index_path="index.json",
        chunk_size=300,
        chunk_overlap=0,
        allowed_extensions=[".md"],
    )
    index.rebuild()
    return index


@pytest.fixture
def index(tmp_path: Path) -> KnowledgeIndex:
    return _make_index(tmp_path)


@pytest.fixture
def client_factory(index):
    """Build a test client whose routes mirror the gateway: the knowledge routes
    followed by the GET-only SPA catch-all."""

    async def _spa(request: web.Request) -> web.Response:
        return web.Response(text="spa")

    def _build() -> TestClient:
        server = MagicMock()
        server._require_api_token = MagicMock(return_value=None)
        server._require_admin_token = MagicMock(return_value=None)
        server._agent_loop.knowledge = index
        api = KnowledgeAPI(server)

        app = web.Application()
        app.router.add_get("/api/v1/knowledge/status", api.get_status)
        app.router.add_get("/api/v1/knowledge/documents", api.list_documents)
        app.router.add_delete("/api/v1/knowledge/documents/{path:.+}", api.delete_document)
        app.router.add_get("/{path:.*}", _spa)
        return TestClient(TestServer(app))

    return _build


@pytest.mark.asyncio
async def test_list_documents_returns_nested_relative_paths(client_factory):
    async with client_factory() as client:
        data = await (await client.get("/api/v1/knowledge/documents")).json()
        paths = {d["path"] for d in data["documents"]}
        assert paths == {"root.md", "sub/nested.md"}


@pytest.mark.asyncio
async def test_delete_nested_document(client_factory, index):
    """The path the list endpoint hands out must be deletable as-is.

    Regression: a single-segment {path} could not match "sub/nested.md", so the
    request fell through to the SPA catch-all (GET only) and answered 405 —
    the UI showed an unexplained failure toast and the file stayed put.
    """
    async with client_factory() as client:
        resp = await client.delete("/api/v1/knowledge/documents/sub/nested.md")
        assert resp.status == 200, await resp.text()
        assert (await resp.json())["status"] == "deleted"
        assert not (index.docs_dir / "sub" / "nested.md").exists()
        assert (index.docs_dir / "root.md").exists()


@pytest.mark.asyncio
async def test_delete_nested_document_percent_encoded(client_factory, index):
    """yarl decodes %2F before routing, so the encoded form must work too."""
    async with client_factory() as client:
        resp = await client.delete("/api/v1/knowledge/documents/sub%2Fnested.md")
        assert resp.status == 200, await resp.text()
        assert not (index.docs_dir / "sub" / "nested.md").exists()


@pytest.mark.asyncio
async def test_delete_top_level_document_still_works(client_factory, index):
    async with client_factory() as client:
        resp = await client.delete("/api/v1/knowledge/documents/root.md")
        assert resp.status == 200
        assert not (index.docs_dir / "root.md").exists()


@pytest.mark.asyncio
async def test_delete_missing_document_is_404(client_factory):
    async with client_factory() as client:
        resp = await client.delete("/api/v1/knowledge/documents/sub/ghost.md")
        assert resp.status == 404


@pytest.mark.asyncio
async def test_delete_rejects_encoded_traversal(client_factory, tmp_path):
    """A tail match widens what reaches the handler, so traversal must still be
    refused by _safe_relative_dest rather than by the router.

    Percent-encoded is the form worth asserting: a literal ``../`` is collapsed
    by the client before the request goes out, so it never reaches the handler,
    while ``%2F`` survives normalization and arrives as a real "../" segment.
    """
    outside = tmp_path / "secret.md"
    outside.write_text("secret", encoding="utf-8")
    async with client_factory() as client:
        resp = await client.delete("/api/v1/knowledge/documents/..%2Fsecret.md")
        assert resp.status == 400
        assert outside.exists()


@pytest.mark.asyncio
async def test_delete_rejects_absolute_path(client_factory, tmp_path):
    outside = tmp_path / "secret2.md"
    outside.write_text("secret", encoding="utf-8")
    async with client_factory() as client:
        resp = await client.delete(f"/api/v1/knowledge/documents/{outside}")
        assert resp.status == 400
        assert outside.exists()


@pytest.mark.asyncio
async def test_delete_requires_admin_token(index):
    """The admin guard runs before anything is touched on disk."""
    server = MagicMock()
    server._require_admin_token = MagicMock(
        return_value=web.json_response({"error": "admin authorization required"}, status=403)
    )
    server._agent_loop.knowledge = index
    api = KnowledgeAPI(server)
    app = web.Application()
    app.router.add_delete("/api/v1/knowledge/documents/{path:.+}", api.delete_document)
    async with TestClient(TestServer(app)) as client:
        resp = await client.delete("/api/v1/knowledge/documents/sub/nested.md")
        assert resp.status == 403
        assert (index.docs_dir / "sub" / "nested.md").exists()


@pytest.mark.asyncio
async def test_status_exposes_fields_the_dashboard_reads(client_factory):
    """The dashboard status line reads documents/chunks/stale/last_rebuild.

    Regression: it used to read indexed_count/last_rebuild, and status()
    returned neither, so the line was pinned at "0 indexed · never".
    """
    async with client_factory() as client:
        status = await (await client.get("/api/v1/knowledge/status")).json()
        assert status["documents"] == 2
        assert status["chunks"] >= 2
        assert status["stale"] is False
        assert isinstance(status["last_rebuild"], str) and status["last_rebuild"]


def test_status_last_rebuild_is_none_before_first_build(tmp_path: Path):
    docs = tmp_path / "docs"
    docs.mkdir()
    index = KnowledgeIndex(
        workspace=tmp_path,
        docs_dir="docs",
        index_path="index.json",
        allowed_extensions=[".md"],
    )
    assert index.status()["last_rebuild"] is None


def test_status_last_rebuild_tracks_index_mtime(tmp_path: Path):
    index = _make_index(tmp_path)
    first = index.status()["last_rebuild"]
    # Bump the index mtime deterministically instead of sleeping for the
    # filesystem's timestamp granularity.
    stat = index.index_path.stat()
    os.utime(index.index_path, (stat.st_atime, stat.st_mtime + 120))
    assert index.status()["last_rebuild"] > first


@pytest.mark.asyncio
async def test_status_404_when_index_not_configured():
    server = MagicMock()
    server._require_api_token = MagicMock(return_value=None)
    server._agent_loop.knowledge = None
    api = KnowledgeAPI(server)
    app = web.Application()
    app.router.add_get("/api/v1/knowledge/status", api.get_status)
    async with TestClient(TestServer(app)) as client:
        resp = await client.get("/api/v1/knowledge/status")
        assert resp.status == 404


@pytest.mark.asyncio
async def test_delete_triggers_reindex(index):
    """Deleting a document rebuilds the index so search stops returning it."""
    server = MagicMock()
    server._require_admin_token = MagicMock(return_value=None)
    server._agent_loop.knowledge = index
    api = KnowledgeAPI(server)
    app = web.Application()
    app.router.add_delete("/api/v1/knowledge/documents/{path:.+}", api.delete_document)
    async with TestClient(TestServer(app)) as client:
        assert index.search("subdir") != []
        resp = await client.delete("/api/v1/knowledge/documents/sub/nested.md")
        assert resp.status == 200
        assert index.search("subdir") == []
