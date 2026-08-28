"""Regression: 四处写权 provenance 旁路（工具 replace/remove、reviewer remove、
REST update/delete）必须被 provenance_guard 拦截。

旁路根因：这些入口直接调 store.update/delete，而 store 层不做来源分级判定，
低优先级来源（model_inferred / admin 无 override）可覆盖或删除 user_stated 条目。
被拒行为（S3 期）：仅返回结构化拒绝，不打 tag、不写 contradiction 行。
"""

from __future__ import annotations

import json
from pathlib import Path
from unittest.mock import AsyncMock, MagicMock

import pytest

from echo_agent.agent.tools.memory import MemoryTool
from echo_agent.memory.reviewer import MemoryReviewer
from echo_agent.memory.service import MemoryService
from echo_agent.memory.store import MemoryStore
from echo_agent.memory.types import MemoryEntry, MemoryType
from echo_agent.tools import ToolExecutionContext


def _store(tmp_path: Path) -> MemoryStore:
    return MemoryStore(tmp_path / "mem")


def _user_entry(s: MemoryStore) -> MemoryEntry:
    return s.add(MemoryEntry(type=MemoryType.USER, key="home", content="上海", source="user_stated"))


# ── 工具 _remove / _replace ──────────────────────────────────────────────────

# 工具已改走 service:入口是 async execute + 含 scope 的 ctx(model actor)。
_CTX = ToolExecutionContext(session_key="sess", memory_scope="sess")


@pytest.mark.asyncio
async def test_tool_remove_cannot_delete_user_stated(tmp_path):
    s = _store(tmp_path)
    e = _user_entry(s)
    tool = MemoryTool(service=MemoryService(s))  # 默认 model_inferred actor
    res = await tool.execute({"action": "remove", "target": "user", "key": "home"}, _CTX)
    assert res.success is False
    assert s.get(e.id) is not None


@pytest.mark.asyncio
async def test_tool_replace_cannot_overwrite_user_stated(tmp_path):
    s = _store(tmp_path)
    e = _user_entry(s)
    tool = MemoryTool(service=MemoryService(s))
    res = await tool.execute(
        {"action": "replace", "target": "user", "key": "home", "content": "北京", "source": "model_inferred"},
        _CTX,
    )
    assert res.success is False
    assert s.get(e.id).content == "上海"


@pytest.mark.asyncio
async def test_tool_remove_被拒不打tag不写contradiction(tmp_path):
    s = _store(tmp_path)
    e = _user_entry(s)
    tool = MemoryTool(service=MemoryService(s))
    await tool.execute({"action": "remove", "target": "user", "key": "home"}, _CTX)
    kept = s.get(e.id)
    assert MemoryStore.SUSPECTED_CONFLICT_TAG not in kept.tags


# ── reviewer remove ──────────────────────────────────────────────────────────

@pytest.mark.asyncio
async def test_reviewer_remove_cannot_delete_user_stated(tmp_path):
    s = _store(tmp_path)
    e = _user_entry(s)
    reviewer = MemoryReviewer(provider=MagicMock(), service=MemoryService(s), session_key="sess")
    result = await reviewer._execute({"action": "remove", "target": "user", "key": "home"})
    assert not result.startswith("Removed")
    assert s.get(e.id) is not None


# ── REST update / delete ─────────────────────────────────────────────────────

class _Request:
    def __init__(self, *, body=None, match_info=None, query=None):
        self._body = body if body is not None else {}
        self.match_info = match_info or {}
        self.query = query or {}
        self.headers = {}

    async def json(self):
        return self._body


def _make_api():
    from echo_agent.gateway.api.memory import MemoryAPI

    server = MagicMock()
    server._require_admin_token = MagicMock(return_value=None)
    api = MemoryAPI(server)
    store = MagicMock()
    # flush_pending_embeds 会被 await,须是异步桩(否则 override 放行分支 await MagicMock 抛 TypeError)
    store.flush_pending_embeds = AsyncMock()
    server._agent_loop.memory = store
    server._agent_loop._invalidate_memory_caches = AsyncMock()
    # 端到端走真 MemoryService 的 admin 通道:置空 loop 上的单例槽,使 _service()
    # 就地用上面的 mock store + 异步失效/flush 构造真 service(验证 provenance 真拦)。
    server._agent_loop._memory_service = None
    server._agent_loop.config.memory.allow_model_environment_writes = False
    return api, store


async def _payload(resp):
    return json.loads(resp.body.decode())


@pytest.mark.asyncio
async def test_rest_update_user_stated_denied_without_override(tmp_path):
    api, store = _make_api()
    store.get.return_value = MemoryEntry(
        type=MemoryType.USER, key="home", content="上海", source="user_stated",
        source_session="s",
    )
    resp = await api.update_entry(_Request(match_info={"id": "x"}, body={"content": "北京"}))
    assert resp.status == 403
    store.update.assert_not_called()


@pytest.mark.asyncio
async def test_rest_delete_user_stated_denied_without_override(tmp_path):
    api, store = _make_api()
    store.get.return_value = MemoryEntry(
        type=MemoryType.USER, key="home", content="上海", source="user_stated",
        source_session="s",
    )
    resp = await api.delete_entry(_Request(match_info={"id": "x"}))
    assert resp.status == 403
    store.delete.assert_not_called()


@pytest.mark.asyncio
async def test_rest_update_tags_only_user_stated_denied_without_override(tmp_path):
    # admin 仅改 tags(无 content)也应受 provenance 守卫约束:
    # user_stated 条目、无 override → 被拒 403,且不落库(store.update 不被调用)。
    api, store = _make_api()
    store.get.return_value = MemoryEntry(
        type=MemoryType.USER, key="home", content="上海", source="user_stated",
        source_session="s",
    )
    resp = await api.update_entry(_Request(match_info={"id": "x"}, body={"tags": ["vip"]}))
    assert resp.status == 403
    store.update.assert_not_called()


@pytest.mark.asyncio
async def test_rest_update_tags_only_allowed_with_override(tmp_path):
    api, store = _make_api()
    store.get.return_value = MemoryEntry(
        type=MemoryType.USER, key="home", content="上海", source="user_stated",
        source_session="s",
    )
    store.update.return_value = MemoryEntry(
        type=MemoryType.USER, key="home", content="上海", source="user_stated",
        source_session="s", tags=["vip"],
    )
    resp = await api.update_entry(
        _Request(match_info={"id": "x"}, body={"tags": ["vip"], "override": True})
    )
    assert resp.status == 200
    store.update.assert_called_once()


@pytest.mark.asyncio
async def test_rest_update_user_stated_allowed_with_override(tmp_path):
    api, store = _make_api()
    store.get.return_value = MemoryEntry(
        type=MemoryType.USER, key="home", content="上海", source="user_stated",
        source_session="s",
    )
    store.update.return_value = MemoryEntry(
        type=MemoryType.USER, key="home", content="北京", source="user_stated",
        source_session="s",
    )
    resp = await api.update_entry(
        _Request(match_info={"id": "x"}, body={"content": "北京", "override": True})
    )
    assert resp.status == 200
    store.update.assert_called_once()


@pytest.mark.asyncio
async def test_rest_delete_user_stated_allowed_with_override(tmp_path):
    api, store = _make_api()
    store.get.return_value = MemoryEntry(
        type=MemoryType.USER, key="home", content="上海", source="user_stated",
        source_session="s",
    )
    store.delete.return_value = True
    resp = await api.delete_entry(
        _Request(match_info={"id": "x"}, query={"override": "true"})
    )
    assert resp.status == 200
    store.delete.assert_called_once()
