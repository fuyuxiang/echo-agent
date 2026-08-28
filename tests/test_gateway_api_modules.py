"""Unit tests for gateway management API handlers.

Covers auth guard checks, request validation, error responses (4xx/5xx),
and success response structure for the skills/knowledge/memory/config/channels
management routes. All collaborators (skill store, memory store, knowledge
index, config loader, channel manager) are mocked.
"""

from __future__ import annotations

import json
from unittest.mock import AsyncMock, MagicMock

import pytest
from aiohttp import web


# REGRESSION GUARD: the API modules must guard with `if guard is not None:`, not
# `if guard:`. An aiohttp Response is falsy (its __len__ returns len(self._state)
# == 0), so `if guard:` silently drops the 401 Response from _require_api_token
# and bypasses token auth. The *_unauthorized tests below assert the 401 is
# actually returned — they pin the fix so the bypass cannot regress.


# ══════════════════════════════════════════════════════════════════════════════
# Test helpers
# ══════════════════════════════════════════════════════════════════════════════


def _make_server():
    """A mock GatewayServer. Token guards return None (authorized)."""
    server = MagicMock()
    server._require_api_token = MagicMock(return_value=None)
    server._require_admin_token = MagicMock(return_value=None)
    return server


def _unauthorized_server():
    """A mock server whose token guards reject every request with 401."""
    server = _make_server()
    rejection = web.json_response({"error": "unauthorized"}, status=401)
    server._require_api_token = MagicMock(return_value=rejection)
    server._require_admin_token = MagicMock(return_value=rejection)
    return server


class _Request:
    """Minimal aiohttp.Request stand-in."""

    def __init__(self, *, body=None, raise_json=None, match_info=None, query=None,
                 multipart=None):
        self._body = body if body is not None else {}
        self._raise_json = raise_json
        self.match_info = match_info or {}
        self.query = query or {}
        self.headers = {}
        self._multipart = multipart

    async def json(self):
        if self._raise_json is not None:
            raise self._raise_json
        return self._body

    async def multipart(self):
        return self._multipart


async def _payload(response: web.Response) -> dict:
    """Decode a json_response body into a dict."""
    return json.loads(response.body.decode())


class _Part:
    """A fake multipart body part exposing read_chunk()."""

    def __init__(self, *, name, filename, chunks):
        self.name = name
        self.filename = filename
        self._chunks = list(chunks) + [b""]  # trailing empty signals EOF

    async def read_chunk(self, size=8192):
        return self._chunks.pop(0) if self._chunks else b""


class _MultipartReader:
    """A fake multipart reader yielding parts via next()."""

    def __init__(self, parts):
        self._parts = list(parts) + [None]  # trailing None signals end

    async def next(self):
        return self._parts.pop(0) if self._parts else None


def _mem_entry(eid="m1", tier="working", mtype="user"):
    entry = MagicMock()
    entry.tier.value = tier
    entry.type.value = mtype
    entry.to_dict.return_value = {"id": eid, "tier": tier, "type": mtype}
    return entry


# ══════════════════════════════════════════════════════════════════════════════
# SkillsAPI
# ══════════════════════════════════════════════════════════════════════════════


class TestSkillsAPI:
    def _make(self, server=None):
        from echo_agent.gateway.api.skills import SkillsAPI

        server = server or _make_server()
        api = SkillsAPI(server)
        store = MagicMock()
        server._agent_loop.skill_store = store
        return api, store, server

    @pytest.mark.asyncio
    async def test_list_skills_unauthorized(self):
        from echo_agent.gateway.api.skills import SkillsAPI

        server = _unauthorized_server()
        api = SkillsAPI(server)
        resp = await api.list_skills(_Request())
        assert resp.status == 401

    @pytest.mark.asyncio
    async def test_list_skills_success(self):
        api, store, _ = self._make()
        meta = MagicMock()
        meta.name = "alpha"
        meta.to_dict.return_value = {"name": "alpha"}
        store.list_all.return_value = [meta]
        store.is_disabled.return_value = False

        resp = await api.list_skills(_Request())
        assert resp.status == 200
        data = await _payload(resp)
        assert data["skills"][0]["name"] == "alpha"
        assert data["skills"][0]["enabled"] is True

    @pytest.mark.asyncio
    async def test_get_skill_not_found(self):
        api, store, _ = self._make()
        store.read_skill.return_value = None
        resp = await api.get_skill(_Request(match_info={"name": "ghost"}))
        assert resp.status == 404

    @pytest.mark.asyncio
    async def test_get_skill_success(self):
        api, store, _ = self._make()
        store.read_skill.return_value = "# content"
        store.list_files.return_value = ["a.py"]
        resp = await api.get_skill(_Request(match_info={"name": "alpha"}))
        assert resp.status == 200
        data = await _payload(resp)
        assert data["name"] == "alpha"
        assert data["content"] == "# content"
        assert data["files"] == ["a.py"]

    @pytest.mark.asyncio
    async def test_toggle_skill_enable(self):
        api, store, _ = self._make()
        store.is_disabled.return_value = True
        resp = await api.toggle_skill(_Request(match_info={"name": "alpha"}))
        data = await _payload(resp)
        assert data["success"] is True
        assert data["skill"]["enabled"] is True
        store.persist_enable.assert_called_once_with("alpha")

    @pytest.mark.asyncio
    async def test_toggle_skill_disable(self):
        api, store, _ = self._make()
        store.is_disabled.return_value = False
        resp = await api.toggle_skill(_Request(match_info={"name": "alpha"}))
        data = await _payload(resp)
        assert data["skill"]["enabled"] is False
        store.persist_disable.assert_called_once_with("alpha")

    @pytest.mark.asyncio
    async def test_delete_skill_error(self):
        api, store, _ = self._make()
        store.delete_skill.return_value = "cannot delete builtin"
        resp = await api.delete_skill(_Request(match_info={"name": "alpha"}))
        assert resp.status == 400
        data = await _payload(resp)
        assert data["error"] == "cannot delete builtin"

    @pytest.mark.asyncio
    async def test_delete_skill_success(self):
        api, store, _ = self._make()
        store.delete_skill.return_value = None
        resp = await api.delete_skill(_Request(match_info={"name": "alpha"}))
        assert resp.status == 200
        data = await _payload(resp)
        assert data["success"] is True
        # Clearing the disable entries is now SkillStore.delete_skill's job, so
        # the API layer only has to delegate.
        store.delete_skill.assert_called_once_with("alpha")

    @pytest.mark.asyncio
    async def test_import_skill_invalid_json(self):
        api, _, _ = self._make()
        resp = await api.import_skill(_Request(raise_json=ValueError("bad")))
        assert resp.status == 400
        data = await _payload(resp)
        assert "invalid JSON" in data["error"]

    @pytest.mark.asyncio
    async def test_import_skill_missing_path(self):
        api, _, _ = self._make()
        resp = await api.import_skill(_Request(body={}))
        assert resp.status == 400
        data = await _payload(resp)
        assert "path is required" in data["error"]

    @pytest.mark.asyncio
    async def test_import_skill_no_skill_md(self, tmp_path):
        api, _, _ = self._make()
        resp = await api.import_skill(_Request(body={"path": str(tmp_path)}))
        assert resp.status == 400
        data = await _payload(resp)
        assert "no SKILL.md" in data["error"]

    @pytest.mark.asyncio
    async def test_import_skill_parse_failure(self, tmp_path):
        api, store, _ = self._make()
        (tmp_path / "SKILL.md").write_text("x")
        store._read_meta.return_value = None
        resp = await api.import_skill(_Request(body={"path": str(tmp_path)}))
        assert resp.status == 400
        data = await _payload(resp)
        assert "failed to parse" in data["error"]

    @pytest.mark.asyncio
    async def test_import_skill_already_exists(self, tmp_path):
        api, store, _ = self._make()
        src = tmp_path / "src"
        src.mkdir()
        (src / "SKILL.md").write_text("x")
        meta = MagicMock()
        meta.name = "alpha"
        meta.category = "general"
        store._read_meta.return_value = meta
        existing = tmp_path / "user" / "general" / "alpha"
        existing.mkdir(parents=True)
        store.user_dir = tmp_path / "user"
        resp = await api.import_skill(_Request(body={"path": str(src)}))
        assert resp.status == 409
        data = await _payload(resp)
        assert "already exists" in data["error"]

    @pytest.mark.asyncio
    async def test_import_skill_success(self, tmp_path):
        api, store, _ = self._make()
        src = tmp_path / "src"
        src.mkdir()
        (src / "SKILL.md").write_text("x")
        meta = MagicMock()
        meta.name = "alpha"
        meta.category = "general"
        meta.to_dict.return_value = {"name": "alpha"}
        store._read_meta.return_value = meta
        store.user_dir = tmp_path / "user"
        resp = await api.import_skill(_Request(body={"path": str(src)}))
        assert resp.status == 200
        data = await _payload(resp)
        assert data["success"] is True
        assert data["skill"]["enabled"] is False


# ══════════════════════════════════════════════════════════════════════════════
# SkillsAPI deps (体检 + 安装)
# ══════════════════════════════════════════════════════════════════════════════


class TestSkillsDeps:
    def _make(self):
        from echo_agent.gateway.api.skills import SkillsAPI
        server = MagicMock()
        server._require_api_token = MagicMock(return_value=None)
        server._require_admin_token = MagicMock(return_value=None)
        store = MagicMock()
        server._agent_loop.skill_store = store
        return SkillsAPI(server), store, server

    @pytest.mark.asyncio
    async def test_get_deps_reads_requires_pip(self):
        api, store, _ = self._make()
        store.read_skill.return_value = (
            "---\nname: ppt-author\nmetadata:\n  echo:\n"
            "    requires:\n      pip: [python-pptx]\n---\nbody"
        )
        import echo_agent.dependencies.lazy_deps as ld
        orig = ld._is_satisfied
        ld._is_satisfied = lambda s: False
        try:
            resp = await api.get_skill_deps(_Request(match_info={"name": "ppt-author"}))
        finally:
            ld._is_satisfied = orig
        assert resp.status == 200
        data = await _payload(resp)
        assert "python-pptx" in data["requires"]
        assert "python-pptx" in data["missing"]
        assert data["satisfied"] is False

    @pytest.mark.asyncio
    async def test_get_deps_not_found(self):
        api, store, _ = self._make()
        store.read_skill.return_value = None
        resp = await api.get_skill_deps(_Request(match_info={"name": "nope"}))
        assert resp.status == 404

    @pytest.mark.asyncio
    async def test_get_deps_unauthorized(self):
        from echo_agent.gateway.api.skills import SkillsAPI
        server = _unauthorized_server()
        api = SkillsAPI(server)
        resp = await api.get_skill_deps(_Request(match_info={"name": "x"}))
        assert resp.status == 401

    @pytest.mark.asyncio
    async def test_install_deps_calls_install_authorized(self, monkeypatch):
        api, store, _ = self._make()
        store.read_skill.return_value = (
            "---\nname: ppt-author\nmetadata:\n  echo:\n"
            "    requires:\n      pip: [python-pptx]\n---\nbody"
        )
        import echo_agent.dependencies.lazy_deps as ld
        monkeypatch.setattr(ld, "install_authorized",
                            lambda specs, *, source: {"success": True, "installed": list(specs),
                                                      "skipped": [], "rejected": [], "detail": "ok"})
        resp = await api.install_skill_deps(_Request(match_info={"name": "ppt-author"}, body={}))
        assert resp.status == 200
        data = await _payload(resp)
        assert data["success"] is True
        assert "python-pptx" in data["installed"]


# ══════════════════════════════════════════════════════════════════════════════
# KnowledgeAPI
# ══════════════════════════════════════════════════════════════════════════════


class TestKnowledgeAPI:
    def _make(self, index="__default__"):
        from echo_agent.gateway.api.knowledge import KnowledgeAPI

        server = _make_server()
        api = KnowledgeAPI(server)
        if index == "__default__":
            index = MagicMock()
            # Runtime rebuilds go through async rebuild_async (refreshes vectors).
            index.rebuild_async = AsyncMock(return_value={})
        server._agent_loop.knowledge = index
        return api, index, server

    @pytest.mark.asyncio
    async def test_get_status_unauthorized(self):
        from echo_agent.gateway.api.knowledge import KnowledgeAPI

        api = KnowledgeAPI(_unauthorized_server())
        resp = await api.get_status(_Request())
        assert resp.status == 401

    @pytest.mark.asyncio
    async def test_get_status_not_configured(self):
        api, _, _ = self._make(index=None)
        resp = await api.get_status(_Request())
        assert resp.status == 404
        data = await _payload(resp)
        assert "not configured" in data["error"]

    @pytest.mark.asyncio
    async def test_get_status_success(self):
        api, index, _ = self._make()
        index.status.return_value = {"docs": 5}
        resp = await api.get_status(_Request())
        assert resp.status == 200
        data = await _payload(resp)
        assert data["docs"] == 5

    @pytest.mark.asyncio
    async def test_rebuild_not_configured(self):
        api, _, _ = self._make(index=None)
        resp = await api.rebuild(_Request())
        assert resp.status == 404

    @pytest.mark.asyncio
    async def test_rebuild_success(self):
        api, index, _ = self._make()
        index.rebuild_async.return_value = {"indexed": 3}
        resp = await api.rebuild(_Request())
        assert resp.status == 200
        data = await _payload(resp)
        assert data["indexed"] == 3

    @pytest.mark.asyncio
    async def test_list_documents_not_configured(self):
        api, _, _ = self._make(index=None)
        resp = await api.list_documents(_Request())
        assert resp.status == 404

    @pytest.mark.asyncio
    async def test_list_documents_no_dir(self, tmp_path):
        api, index, _ = self._make()
        index.status.return_value = {"docs_dir": str(tmp_path / "missing")}
        resp = await api.list_documents(_Request())
        assert resp.status == 200
        data = await _payload(resp)
        assert data["documents"] == []

    @pytest.mark.asyncio
    async def test_list_documents_success(self, tmp_path):
        api, index, _ = self._make()
        (tmp_path / "doc.txt").write_text("hello")
        (tmp_path / ".hidden").write_text("x")
        index.status.return_value = {"docs_dir": str(tmp_path)}
        resp = await api.list_documents(_Request())
        data = await _payload(resp)
        names = [d["path"] for d in data["documents"]]
        assert "doc.txt" in names
        assert ".hidden" not in names

    @pytest.mark.asyncio
    async def test_delete_document_not_configured(self):
        api, _, _ = self._make(index=None)
        resp = await api.delete_document(_Request(match_info={"path": "a.txt"}))
        assert resp.status == 404

    @pytest.mark.asyncio
    async def test_delete_document_invalid_path(self, tmp_path):
        api, index, _ = self._make()
        index.status.return_value = {"docs_dir": str(tmp_path)}
        resp = await api.delete_document(
            _Request(match_info={"path": "../../etc/passwd"})
        )
        assert resp.status == 400
        data = await _payload(resp)
        assert "invalid path" in data["error"]

    @pytest.mark.asyncio
    async def test_delete_document_not_found(self, tmp_path):
        api, index, _ = self._make()
        index.status.return_value = {"docs_dir": str(tmp_path)}
        resp = await api.delete_document(_Request(match_info={"path": "ghost.txt"}))
        assert resp.status == 404

    @pytest.mark.asyncio
    async def test_delete_document_success(self, tmp_path):
        api, index, _ = self._make()
        target = tmp_path / "doc.txt"
        target.write_text("x")
        index.status.return_value = {"docs_dir": str(tmp_path)}
        index.rebuild_async.return_value = {"indexed": 0}
        resp = await api.delete_document(_Request(match_info={"path": "doc.txt"}))
        assert resp.status == 200
        data = await _payload(resp)
        assert data["status"] == "deleted"
        assert not target.exists()

    @pytest.mark.asyncio
    async def test_upload_not_configured(self):
        api, _, _ = self._make(index=None)
        resp = await api.upload(_Request())
        assert resp.status == 404

    @pytest.mark.asyncio
    async def test_upload_no_multipart(self):
        api, index, _ = self._make()
        resp = await api.upload(_Request(multipart=None))
        assert resp.status == 400
        data = await _payload(resp)
        assert "multipart form required" in data["error"]

    @pytest.mark.asyncio
    async def test_upload_no_files(self, tmp_path):
        api, index, _ = self._make()
        index.status.return_value = {"docs_dir": str(tmp_path)}
        reader = _MultipartReader([])
        resp = await api.upload(_Request(multipart=reader))
        assert resp.status == 400
        data = await _payload(resp)
        assert "no files uploaded" in data["error"]

    @pytest.mark.asyncio
    async def test_upload_success(self, tmp_path):
        api, index, _ = self._make()
        docs_dir = tmp_path / "docs"
        index.status.return_value = {"docs_dir": str(docs_dir)}
        index.rebuild_async.return_value = {"indexed": 1}
        part = _Part(name="file", filename="note.txt", chunks=[b"hello ", b"world"])
        reader = _MultipartReader([part])
        resp = await api.upload(_Request(multipart=reader))
        assert resp.status == 200
        data = await _payload(resp)
        assert data["uploaded"] == ["note.txt"]
        assert data["index"]["indexed"] == 1
        assert (docs_dir / "note.txt").read_bytes() == b"hello world"


# ══════════════════════════════════════════════════════════════════════════════
# MemoryAPI
# ══════════════════════════════════════════════════════════════════════════════


class TestMemoryAPI:
    def _make(self):
        from echo_agent.gateway.api.memory import MemoryAPI

        server = _make_server()
        api = MemoryAPI(server)
        store = MagicMock()
        # 写后失效依赖两个 async 协作者:store.flush_pending_embeds 与
        # loop._invalidate_memory_caches,默认挂 AsyncMock 以便 await 成功。
        store.flush_pending_embeds = AsyncMock(return_value=0)
        server._agent_loop.memory = store
        server._agent_loop._invalidate_memory_caches = AsyncMock()
        return api, store, server

    def _make_with_service(self):
        """写端点(update/delete)已改走 MemoryService 的 admin 通道:
        server 上挂 mock service,store 仅供 handler 读目标条目派生 scope。"""
        from echo_agent.gateway.api.memory import MemoryAPI

        server = _make_server()
        service = MagicMock()
        server._agent_loop._memory_service = service
        store = MagicMock()
        store.get.return_value = _mem_entry("x")
        store.flush_pending_embeds = AsyncMock(return_value=0)
        server._agent_loop.memory = store
        server._agent_loop._invalidate_memory_caches = AsyncMock()
        api = MemoryAPI(server)
        return api, service, store, server

    @staticmethod
    def _write_result(ok=True, reason="", entry_id="x"):
        from echo_agent.memory.service import WriteResult

        return WriteResult(ok=ok, entry=_mem_entry(entry_id) if ok else None, reason=reason)

    @pytest.mark.asyncio
    async def test_list_entries_unauthorized(self):
        from echo_agent.gateway.api.memory import MemoryAPI

        api = MemoryAPI(_unauthorized_server())
        resp = await api.list_entries(_Request())
        assert resp.status == 401

    @pytest.mark.asyncio
    async def test_list_entries_success_with_pagination(self):
        api, store, _ = self._make()
        store.list_all.return_value = [_mem_entry(f"m{i}") for i in range(5)]
        resp = await api.list_entries(
            _Request(query={"offset": "1", "limit": "2"})
        )
        assert resp.status == 200
        data = await _payload(resp)
        assert data["total"] == 5
        assert data["offset"] == 1
        assert data["limit"] == 2
        assert len(data["entries"]) == 2

    @pytest.mark.asyncio
    async def test_list_entries_tier_filter(self):
        api, store, _ = self._make()
        store.list_all.return_value = [
            _mem_entry("a", tier="working"),
            _mem_entry("b", tier="episodic"),
        ]
        resp = await api.list_entries(_Request(query={"tier": "episodic"}))
        data = await _payload(resp)
        assert data["total"] == 1
        assert data["entries"][0]["id"] == "b"

    @pytest.mark.asyncio
    async def test_list_entries_type_filter(self):
        api, store, _ = self._make()
        store.list_all.return_value = [_mem_entry("a")]
        resp = await api.list_entries(_Request(query={"type": "user"}))
        assert resp.status == 200
        _, kwargs = store.list_all.call_args
        assert kwargs["mem_type"] is not None

    @pytest.mark.asyncio
    async def test_stats_success(self):
        api, store, _ = self._make()
        store.list_all.return_value = [
            _mem_entry("a", tier="working", mtype="user"),
            _mem_entry("b", tier="working", mtype="environment"),
        ]
        resp = await api.stats(_Request())
        data = await _payload(resp)
        assert data["total"] == 2
        assert data["by_tier"]["working"] == 2
        assert data["by_type"]["user"] == 1
        assert data["by_type"]["environment"] == 1

    @pytest.mark.asyncio
    async def test_get_entry_not_found(self):
        api, store, _ = self._make()
        store.get.return_value = None
        resp = await api.get_entry(_Request(match_info={"id": "x"}))
        assert resp.status == 404

    @pytest.mark.asyncio
    async def test_get_entry_success(self):
        api, store, _ = self._make()
        store.get.return_value = _mem_entry("x")
        resp = await api.get_entry(_Request(match_info={"id": "x"}))
        assert resp.status == 200
        data = await _payload(resp)
        assert data["id"] == "x"

    @pytest.mark.asyncio
    async def test_update_entry_invalid_json(self):
        api, _, _ = self._make()
        resp = await api.update_entry(
            _Request(match_info={"id": "x"}, raise_json=json.JSONDecodeError("e", "d", 0))
        )
        assert resp.status == 400

    @pytest.mark.asyncio
    async def test_update_entry_not_found(self):
        api, service, store, _ = self._make_with_service()
        store.get.return_value = None
        resp = await api.update_entry(
            _Request(match_info={"id": "x"}, body={"content": "c"})
        )
        assert resp.status == 404

    @pytest.mark.asyncio
    async def test_update_entry_success(self):
        api, service, store, _ = self._make_with_service()
        service.replace = AsyncMock(return_value=self._write_result())
        resp = await api.update_entry(
            _Request(match_info={"id": "x"}, body={"content": "c", "tags": ["t"]})
        )
        assert resp.status == 200
        data = await _payload(resp)
        assert data["id"] == "x"

    @pytest.mark.asyncio
    async def test_rest_update_passes_override_to_service(self):
        api, service, store, _ = self._make_with_service()
        service.replace = AsyncMock(return_value=self._write_result())
        await api.update_entry(
            _Request(match_info={"id": "x"}, body={"content": "x", "override": True})
        )
        _, kw = service.replace.call_args
        assert kw.get("override") is True
        assert kw.get("source") == "admin"

    @pytest.mark.asyncio
    async def test_rest_update_admin_rejected_without_override(self):
        api, service, store, _ = self._make_with_service()
        service.replace = AsyncMock(
            return_value=self._write_result(ok=False, reason="rejected_provenance")
        )
        resp = await api.update_entry(
            _Request(match_info={"id": "x"}, body={"content": "c"})
        )
        assert resp.status == 403
        _, kw = service.replace.call_args
        assert kw.get("override") is False

    @pytest.mark.asyncio
    async def test_rest_update_invalid_content_maps_400(self):
        api, service, store, _ = self._make_with_service()
        service.replace = AsyncMock(
            return_value=self._write_result(ok=False, reason="invalid")
        )
        resp = await api.update_entry(
            _Request(match_info={"id": "x"}, body={"content": "c"})
        )
        assert resp.status == 400

    @pytest.mark.asyncio
    async def test_rest_update_tags_only_uses_maintenance_update(self):
        # 只传 tags 无 content:走 maintenance_update(不受 provenance 守卫)。
        api, service, store, _ = self._make_with_service()
        service.maintenance_update = AsyncMock(return_value=self._write_result())
        service.replace = AsyncMock(return_value=self._write_result())
        resp = await api.update_entry(
            _Request(match_info={"id": "x"}, body={"tags": ["t"]})
        )
        assert resp.status == 200
        service.maintenance_update.assert_awaited_once()
        service.replace.assert_not_awaited()
        _, kw = service.maintenance_update.call_args
        assert kw.get("tags") == ["t"]

    @pytest.mark.asyncio
    async def test_delete_entry_not_found(self):
        api, service, store, _ = self._make_with_service()
        store.get.return_value = None
        resp = await api.delete_entry(_Request(match_info={"id": "x"}))
        assert resp.status == 404

    @pytest.mark.asyncio
    async def test_delete_entry_success(self):
        api, service, store, _ = self._make_with_service()
        service.remove = AsyncMock(return_value=self._write_result())
        resp = await api.delete_entry(_Request(match_info={"id": "x"}))
        assert resp.status == 200
        data = await _payload(resp)
        assert data["status"] == "deleted"

    @pytest.mark.asyncio
    async def test_rest_delete_passes_override_from_query(self):
        api, service, store, _ = self._make_with_service()
        service.remove = AsyncMock(return_value=self._write_result())
        await api.delete_entry(
            _Request(match_info={"id": "x"}, query={"override": "true"})
        )
        _, kw = service.remove.call_args
        assert kw.get("override") is True

    @pytest.mark.asyncio
    async def test_rest_delete_admin_rejected_without_override(self):
        api, service, store, _ = self._make_with_service()
        service.remove = AsyncMock(
            return_value=self._write_result(ok=False, reason="rejected_provenance")
        )
        resp = await api.delete_entry(_Request(match_info={"id": "x"}))
        assert resp.status == 403
        _, kw = service.remove.call_args
        assert kw.get("override") is False

    @pytest.mark.asyncio
    async def test_search_invalid_json(self):
        api, _, _ = self._make()
        resp = await api.search(
            _Request(raise_json=json.JSONDecodeError("e", "d", 0))
        )
        assert resp.status == 400

    @pytest.mark.asyncio
    async def test_search_missing_query(self):
        api, _, _ = self._make()
        resp = await api.search(_Request(body={}))
        assert resp.status == 400
        data = await _payload(resp)
        assert "query required" in data["error"]

    @pytest.mark.asyncio
    async def test_search_success(self):
        api, store, _ = self._make()
        store.search_scored.return_value = [(_mem_entry("x"), 0.9)]
        resp = await api.search(_Request(body={"query": "hello", "all_scopes": True}))
        assert resp.status == 200
        data = await _payload(resp)
        assert data["results"][0]["score"] == 0.9
        assert data["results"][0]["entry"]["id"] == "x"

    @pytest.mark.asyncio
    async def test_memory_reads_use_api_token(self):
        """按 scope 限定的只读端点走 chat 级 api token。此前读也要 admin token,
        导致配置了独立 admin_tokens 的部署下:用 api token 登录成功(登录探的是
        /stats),整个 Memory 页与概览的记忆计数却全部 403。

        注意必须带 session_key:不带即跨主体读,那条路径由 cross_scope 守卫收回
        admin(见 test_cross_scope_reads_require_admin_token)。stats 例外——它只
        返回按 tier/type 分桶的计数,不含条目内容或 id。"""
        from echo_agent.gateway.api.memory import MemoryAPI
        server = _make_server()
        called: dict[str, list[str]] = {}

        def _api(request, action):
            called.setdefault("api", []).append(action)
            return None

        server._require_api_token = _api
        server._require_admin_token = MagicMock(
            side_effect=AssertionError("scope 内只读端点不得要求 admin token")
        )
        store = MagicMock()
        store.list_all.return_value = []
        store.search_scored.return_value = []
        store.get_stats.return_value = {}
        server._agent_loop.memory = store
        api = MemoryAPI(server)

        await api.list_entries(_Request(query={"session_key": "u1"}))
        await api.stats(_Request(query={}))
        await api.search(_Request(body={"query": "x", "session_key": "u1"}))
        assert called["api"] == ["memory_list", "memory_stats", "memory_search"]

    @pytest.mark.asyncio
    async def test_cross_scope_reads_require_admin_token(self):
        """不带 session_key 的读跨越全部主体,必须要 admin token。

        Audience 只过滤生命周期状态,不代表调用者身份:store._filtered_entries 仅在
        传了 session_key 时才施加可见性过滤。所以把读守卫降到 api token 后,一个
        普通 token 请求"全部"就能读到其他主体的记忆。all_scopes 原先只是意图声明,
        并不授权任何东西。"""
        from echo_agent.gateway.api.memory import MemoryAPI
        server = _make_server()
        server._require_api_token = MagicMock(return_value=None)
        actions: list[str] = []

        def _admin(request, action):
            actions.append(action)
            return web.json_response({"error": "forbidden"}, status=403)

        server._require_admin_token = _admin
        store = MagicMock()
        store.list_all.return_value = []
        store.search_scored.return_value = []
        server._agent_loop.memory = store
        api = MemoryAPI(server)

        assert (await api.list_entries(_Request(query={}))).status == 403
        assert (await api.search(_Request(body={"query": "x", "all_scopes": True}))).status == 403
        # 按 id 取单条同样是 admin:id 本身推不出应见的 scope,且该路径不过 Audience。
        assert (await api.get_entry(_Request(match_info={"id": "m1"}))).status == 403
        assert actions == [
            "memory_list:cross_scope", "memory_search:cross_scope", "memory_get",
        ]

    @pytest.mark.asyncio
    async def test_memory_writes_require_admin_token(self):
        """写端点仍需 admin token:改写会覆盖 provenance、删除不可逆。"""
        from echo_agent.gateway.api.memory import MemoryAPI
        server = _make_server()
        called: dict[str, list[str]] = {}

        def _admin(request, action):
            called.setdefault("admin", []).append(action)
            return None

        server._require_admin_token = _admin
        server._require_api_token = MagicMock(
            side_effect=AssertionError("写端点不得只要 api token")
        )
        store = MagicMock()
        store.get.return_value = None
        store.delete.return_value = False
        server._agent_loop.memory = store
        api = MemoryAPI(server)

        await api.update_entry(_Request(match_info={"id": "m1"}, body={"content": "c"}))
        await api.delete_entry(_Request(match_info={"id": "m1"}))
        assert called["admin"] == ["memory_update", "memory_delete"]

    @pytest.mark.asyncio
    async def test_include_all_escalates_to_admin_token(self):
        """include_all 会把读扩到 Audience.ADMIN,把刻意不参与检索的条目也吐出来,
        因此这一步单独要 admin token——放宽端点守卫不等于把 admin 视图交给所有
        api token。"""
        from echo_agent.gateway.api.memory import MemoryAPI
        server = _make_server()
        server._require_api_token = MagicMock(return_value=None)
        server._require_admin_token = MagicMock(
            return_value=web.json_response({"error": "forbidden"}, status=403)
        )
        store = MagicMock()
        store.list_all.return_value = []
        server._agent_loop.memory = store
        api = MemoryAPI(server)

        # 带 session_key 以隔离出 include_all 这一道守卫:不带的话先被 cross_scope
        # 守卫挡下,测到的就不是这里要验的升级路径了。
        resp = await api.list_entries(
            _Request(query={"session_key": "u1", "include_all": "true"})
        )
        assert resp.status == 403
        _, kwargs = server._require_admin_token.call_args
        assert kwargs.get("action") == "memory_list:include_all"

    @pytest.mark.asyncio
    async def test_search_include_all_accepts_json_boolean(self):
        """search 的 include_all 来自 JSON body,客户端发的是真布尔 true。

        原先写成 `body.get("include_all") == "true"`,对布尔恒为 False——
        /memory/search 的 admin 全量视图静默失效,既没报错也没生效。list 走 query
        string,`== "true"` 在那边才是对的,两处形态不同必须都能识别。"""
        from echo_agent.gateway.api.memory import MemoryAPI
        server = _make_server()
        server._require_api_token = MagicMock(return_value=None)
        actions: list[str] = []

        def _admin(request, action):
            actions.append(action)
            return web.json_response({"error": "forbidden"}, status=403)

        server._require_admin_token = _admin
        store = MagicMock()
        store.search_scored.return_value = []
        server._agent_loop.memory = store
        api = MemoryAPI(server)

        resp = await api.search(
            _Request(body={"query": "x", "session_key": "u1", "include_all": True})
        )
        assert resp.status == 403
        assert actions == ["memory_search:include_all"]

    @pytest.mark.asyncio
    async def test_search_without_scope_requires_all_scopes(self):
        api, store, _ = self._make()
        # body 无 session_key、无 all_scopes:跨 scope 全量返回是暴露口子,应拒绝
        resp = await api.search(_Request(body={"query": "x"}))
        assert resp.status == 400

    @pytest.mark.asyncio
    async def test_search_passes_session_key(self):
        from echo_agent.gateway.api.memory import MemoryAPI
        server = _make_server()
        store = MagicMock()
        store.search_scored.return_value = []
        server._agent_loop.memory = store
        api = MemoryAPI(server)
        await api.search(_Request(body={"query": "hi", "session_key": "owner"}))
        # search 把 session_key 传给 store 做 scope 过滤
        _, kwargs = store.search_scored.call_args
        assert kwargs.get("session_key") == "owner"


# ══════════════════════════════════════════════════════════════════════════════
# ConfigAPI
# ══════════════════════════════════════════════════════════════════════════════


class TestConfigAPI:
    def _make(self, config="__default__"):
        from echo_agent.gateway.api.config import ConfigAPI

        server = _make_server()
        api = ConfigAPI(server)
        if config == "__default__":
            config = MagicMock()
        server._agent_loop.config = config
        return api, config, server

    @pytest.mark.asyncio
    async def test_get_config_unauthorized(self):
        from echo_agent.gateway.api.config import ConfigAPI

        api = ConfigAPI(_unauthorized_server())
        resp = await api.get_config(_Request())
        assert resp.status == 401

    @pytest.mark.asyncio
    async def test_get_config_requires_admin_token(self):
        from echo_agent.gateway.api.config import ConfigAPI
        server = _make_server()
        called = {}
        def _admin(request, action):
            called["action"] = action
            return None
        server._require_admin_token = _admin
        server._require_api_token = MagicMock(side_effect=AssertionError("must not use api token"))
        server._agent_loop.config = MagicMock()
        for f in ("models", "gateway", "session", "memory", "knowledge", "agent", "ui", "evolution"):
            setattr(server._agent_loop.config, f, None)
        api = ConfigAPI(server)
        await api.get_config(_Request())
        assert called["action"] == "config_get"

    @pytest.mark.asyncio
    async def test_get_config_not_available(self):
        api, _, _ = self._make(config=None)
        resp = await api.get_config(_Request())
        assert resp.status == 500
        data = await _payload(resp)
        assert "not available" in data["error"]

    @pytest.mark.asyncio
    async def test_get_config_sanitizes_secrets(self):
        # 真实 config section 是 pydantic 模型:有 model_dump、无 to_dict。用真实
        # 模型驱动 model_dump(mode="json") 路径,既覆盖嵌套子模型序列化(旧 vars()
        # 会抛 TypeError),又验证 snake_case 字段名下的密钥脱敏仍生效。
        from pydantic import BaseModel

        class _Sub(BaseModel):
            timeout: int = 5

        class _Section(BaseModel):
            api_key: str = "supersecret"
            host: str = "x"
            nested: _Sub = _Sub()

        api, config, _ = self._make()
        section = _Section()
        for f in ("models", "gateway", "session", "memory", "knowledge", "agent", "ui", "evolution"):
            setattr(config, f, section if f == "models" else None)
        resp = await api.get_config(_Request())
        assert resp.status == 200
        data = await _payload(resp)
        assert data["models"]["api_key"] == "***"
        assert data["models"]["host"] == "x"
        # 嵌套子模型被递归序列化为原生 dict(旧实现会在此抛 TypeError)
        assert data["models"]["nested"]["timeout"] == 5

def test_sanitize_helpers():
    from echo_agent.gateway.api.config import _sanitize

    assert _sanitize({"password": "p"})["password"] == "***"
    assert _sanitize({"password": ""})["password"] == ""
    assert _sanitize({"nested": {"token": "t"}})["nested"]["token"] == "***"
    assert _sanitize([{"secret": "s"}])[0]["secret"] == "***"
    assert _sanitize("plain") == "plain"
    deep = {"token": "t"}
    assert _sanitize(deep, depth=11) == deep


def test_sanitize_masks_admin_tokens_and_credential_pool():
    from echo_agent.gateway.api.config import _sanitize
    raw = {
        "gateway": {"auth": {"admin_tokens": ["adm-secret"], "api_tokens": ["t"]}},
        "models": {"providers": [{"credential_pool": ["k1", "k2"]}]},
        "extra_headers": {"Authorization": "Bearer xyz", "X-API-Key": "zzz"},
    }
    out = _sanitize(raw)
    assert out["gateway"]["auth"]["admin_tokens"] == "***"
    assert out["models"]["providers"][0]["credential_pool"] == "***"
    assert out["extra_headers"]["Authorization"] == "***"
    assert out["extra_headers"]["X-API-Key"] == "***"


def test_sanitize_masks_all_secret_key_fields():
    from echo_agent.gateway.api.config import _sanitize
    raw = {
        "channels": {"encryption_key": "ek", "encoding_aes_key": "aes",
                     "app_key": "ak", "app_secret": "as"},
        "models": {"providers": [{"fal_key": "fk", "openai_api_key": "ok"}]},
        "gateway": {"auth": {"admin_tokens": ["a"], "api_tokens": ["b"]}},
    }
    out = _sanitize(raw)
    assert out["channels"]["encryption_key"] == "***"
    assert out["channels"]["encoding_aes_key"] == "***"
    assert out["channels"]["app_key"] == "***"
    assert out["channels"]["app_secret"] == "***"
    assert out["models"]["providers"][0]["fal_key"] == "***"
    assert out["models"]["providers"][0]["openai_api_key"] == "***"
    assert out["gateway"]["auth"]["admin_tokens"] == "***"
    assert out["gateway"]["auth"]["api_tokens"] == "***"


def test_sanitize_preserves_non_secret_lookalikes():
    from echo_agent.gateway.api.config import _sanitize
    raw = {
        "memory": {"owner_key": "owner"},
        "agent": {"max_tokens": 65536, "context_window_tokens": 200000,
                  "summary_min_tokens": 100, "summary_max_tokens": 800},
        "gateway": {"token_header": "X-Echo-Agent-Token"},
        "security": {"encryption_key_env": "ECHO_KEY"},
    }
    out = _sanitize(raw)
    assert out["memory"]["owner_key"] == "owner"
    assert out["agent"]["max_tokens"] == 65536
    assert out["agent"]["context_window_tokens"] == 200000
    assert out["agent"]["summary_min_tokens"] == 100
    assert out["agent"]["summary_max_tokens"] == 800
    assert out["gateway"]["token_header"] == "X-Echo-Agent-Token"
    assert out["security"]["encryption_key_env"] == "ECHO_KEY"


# ══════════════════════════════════════════════════════════════════════════════
# ChannelsAPI
# ══════════════════════════════════════════════════════════════════════════════


class TestChannelsAPI:
    def _make(self):
        from echo_agent.gateway.api.channels import ChannelsAPI

        server = _make_server()
        api = ChannelsAPI(server)
        server.channel_manager = MagicMock()
        return api, server

    @pytest.mark.asyncio
    async def test_list_channels_unauthorized(self):
        from echo_agent.gateway.api.channels import ChannelsAPI

        api = ChannelsAPI(_unauthorized_server())
        resp = await api.list_channels(_Request())
        assert resp.status == 401

    @pytest.mark.asyncio
    async def test_list_channels_enabled_and_running(self):
        api, server = self._make()
        server.channel_manager.active_channels = ["telegram"]
        channels_cfg = MagicMock()
        tg = MagicMock()
        tg.enabled = True
        dc = MagicMock()
        dc.enabled = False
        for name in (
            "discord", "webhook", "cli", "cron", "slack", "whatsapp",
            "weixin", "qqbot", "feishu", "dingtalk", "email", "wecom", "matrix",
        ):
            setattr(channels_cfg, name, None)
        channels_cfg.telegram = tg
        channels_cfg.discord = dc
        server._agent_loop.config.channels = channels_cfg

        resp = await api.list_channels(_Request())
        assert resp.status == 200
        data = await _payload(resp)
        names = {c["name"]: c for c in data["channels"]}
        assert names["telegram"]["running"] is True
        assert names["telegram"]["enabled"] is True
        assert names["discord"]["enabled"] is False
        assert names["discord"]["running"] is False

    @pytest.mark.asyncio
    async def test_list_channels_omits_cli(self):
        # 回归：cli 不是常驻投递通道，daemon 网关下 CLIChannel 永不 running，
        # 概览页会把它恒显示为“离线”，误导。即便 cli 已启用且恰好 active，
        # 也不应出现在通道列表里（交互式 CLI 走 /ws，由 health.ws_clients 反映）。
        api, server = self._make()
        server.channel_manager.active_channels = ["cli"]
        channels_cfg = MagicMock()
        cli = MagicMock()
        cli.enabled = True
        for name in (
            "telegram", "discord", "webhook", "cron", "slack", "whatsapp",
            "weixin", "qqbot", "feishu", "dingtalk", "email", "wecom", "matrix",
        ):
            setattr(channels_cfg, name, None)
        channels_cfg.cli = cli
        server._agent_loop.config.channels = channels_cfg

        resp = await api.list_channels(_Request())
        data = await _payload(resp)
        names = {c["name"] for c in data["channels"]}
        assert "cli" not in names

    @pytest.mark.asyncio
    async def test_list_channels_active_not_in_config(self):
        api, server = self._make()
        server.channel_manager.active_channels = ["custom_channel"]
        server._agent_loop.config = None
        resp = await api.list_channels(_Request())
        data = await _payload(resp)
        names = {c["name"] for c in data["channels"]}
        assert "custom_channel" in names

    @pytest.mark.asyncio
    async def test_list_channels_no_config(self):
        api, server = self._make()
        server.channel_manager.active_channels = []
        server._agent_loop.config = None
        resp = await api.list_channels(_Request())
        assert resp.status == 200
        data = await _payload(resp)
        assert data["channels"] == []


# ══════════════════════════════════════════════════════════════════════════════
# Auth regression sweep — every write/destructive handler must reject 401.
# These pin the `if guard is not None:` fix across all guarded entry points.
# ══════════════════════════════════════════════════════════════════════════════


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "module, cls_name, method, kwargs",
    [
        ("skills", "SkillsAPI", "get_skill", {"match_info": {"name": "x"}}),
        ("skills", "SkillsAPI", "toggle_skill", {"match_info": {"name": "x"}}),
        ("skills", "SkillsAPI", "delete_skill", {"match_info": {"name": "x"}}),
        ("skills", "SkillsAPI", "import_skill", {}),
        ("memory", "MemoryAPI", "get_entry", {"match_info": {"id": "m1"}}),
        ("memory", "MemoryAPI", "update_entry", {"match_info": {"id": "m1"}}),
        ("memory", "MemoryAPI", "delete_entry", {"match_info": {"id": "m1"}}),
        ("memory", "MemoryAPI", "search", {}),
        ("memory", "MemoryAPI", "stats", {}),
        ("knowledge", "KnowledgeAPI", "rebuild", {}),
        ("knowledge", "KnowledgeAPI", "upload", {}),
        ("knowledge", "KnowledgeAPI", "delete_document", {"match_info": {"doc_id": "d1"}}),
    ],
)
async def test_write_endpoints_reject_unauthorized(module, cls_name, method, kwargs):
    import importlib

    mod = importlib.import_module(f"echo_agent.gateway.api.{module}")
    api = getattr(mod, cls_name)(_unauthorized_server())
    resp = await getattr(api, method)(_Request(**kwargs))
    assert resp.status == 401
