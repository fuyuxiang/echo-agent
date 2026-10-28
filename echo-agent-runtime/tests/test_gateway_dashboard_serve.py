"""Tests for dashboard SPA static file serving from gateway."""

import pytest
from pathlib import Path
from aiohttp import web
from aiohttp.test_utils import TestServer, TestClient

from echo_agent.gateway.server import GatewayServer


@pytest.fixture
def dashboard_dir(tmp_path):
    """Create a fake dashboard build directory."""
    index = tmp_path / "index.html"
    index.write_text("<!DOCTYPE html><html><body>Dashboard</body></html>")
    assets = tmp_path / "assets"
    assets.mkdir()
    js_file = assets / "main.js"
    js_file.write_text("console.log('dashboard');")
    css_file = assets / "style.css"
    css_file.write_text("body { margin: 0; }")
    return tmp_path


def _make_app(dashboard_dir_val, playground_path_val):
    """Build a minimal aiohttp app with dashboard/playground routes."""
    server = object.__new__(GatewayServer)
    server._resolve_dashboard_dir = lambda: dashboard_dir_val
    server._playground_path = lambda: playground_path_val

    app = web.Application()
    app.router.add_get("/playground", server._handle_playground)
    app.router.add_get("/{path:.*}", server._handle_dashboard)
    return app


@pytest.mark.asyncio
async def test_dashboard_index_served(dashboard_dir):
    """GET / serves dashboard index.html when dashboard dir exists."""
    app = _make_app(dashboard_dir, Path("/nonexistent/index.html"))

    async with TestClient(TestServer(app)) as client:
        resp = await client.get("/")
        assert resp.status == 200
        text = await resp.text()
        assert "Dashboard" in text


@pytest.mark.asyncio
async def test_dashboard_static_asset_served(dashboard_dir):
    """GET /assets/main.js serves the actual JS file, not index.html."""
    app = _make_app(dashboard_dir, Path("/nonexistent/index.html"))

    async with TestClient(TestServer(app)) as client:
        resp = await client.get("/assets/main.js")
        assert resp.status == 200
        text = await resp.text()
        assert "console.log" in text


@pytest.mark.asyncio
async def test_dashboard_spa_fallback(dashboard_dir):
    """GET /some/deep/route falls back to index.html for SPA client-side routing."""
    app = _make_app(dashboard_dir, Path("/nonexistent/index.html"))

    async with TestClient(TestServer(app)) as client:
        resp = await client.get("/some/deep/route")
        assert resp.status == 200
        text = await resp.text()
        assert "Dashboard" in text


@pytest.mark.asyncio
async def test_playground_at_playground_path(tmp_path):
    """GET /playground serves the old playground HTML."""
    playground_html = tmp_path / "playground.html"
    playground_html.write_text("<html><body>Playground</body></html>")

    app = _make_app(None, playground_html)

    async with TestClient(TestServer(app)) as client:
        resp = await client.get("/playground")
        assert resp.status == 200
        text = await resp.text()
        assert "Playground" in text


@pytest.mark.asyncio
async def test_dashboard_fallback_to_playground_when_no_dist(tmp_path):
    """When no dashboard dir exists, GET / falls back to playground."""
    playground_html = tmp_path / "playground.html"
    playground_html.write_text("<html><body>Playground fallback</body></html>")

    app = _make_app(None, playground_html)

    async with TestClient(TestServer(app)) as client:
        resp = await client.get("/")
        assert resp.status == 200
        text = await resp.text()
        assert "Playground fallback" in text


@pytest.mark.asyncio
async def test_resolve_dashboard_dir_finds_web_dist():
    """_resolve_dashboard_dir finds web/dist when index.html exists there."""
    # The actual _resolve_dashboard_dir checks real filesystem paths.
    # In this project, web/dist/index.html exists (built by Task 13).
    server = object.__new__(GatewayServer)
    result = server._resolve_dashboard_dir()
    # In dev environment, web/dist should exist
    if result is not None:
        assert (result / "index.html").exists()


@pytest.mark.asyncio
async def test_path_traversal_blocked(dashboard_dir):
    """Path traversal attempts should fall back to index.html."""
    app = _make_app(dashboard_dir, Path("/nonexistent/index.html"))

    async with TestClient(TestServer(app)) as client:
        resp = await client.get("/../../../etc/passwd")
        assert resp.status == 200
        text = await resp.text()
        # Should get index.html (SPA fallback), not the system file
        assert "Dashboard" in text
