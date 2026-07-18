"""Regression: 四处写权 provenance 旁路（工具 replace/remove、reviewer remove、
REST update/delete）必须被 provenance_guard 拦截。

旁路根因：这些入口直接调 store.update/delete，而 store 层不做来源分级判定，
低优先级来源（model_inferred / admin 无 override）可覆盖或删除 user_stated 条目。
被拒行为（S3 期）：仅返回结构化拒绝，不打 tag、不写 contradiction 行。
"""

from __future__ import annotations

import json
from pathlib import Path
from unittest.mock import MagicMock

import pytest

from echo_agent.agent.tools.memory import MemoryTool
from echo_agent.memory.reviewer import MemoryReviewer
from echo_agent.memory.store import MemoryStore
from echo_agent.memory.types import MemoryEntry, MemoryType


def _store(tmp_path: Path) -> MemoryStore:
    return MemoryStore(tmp_path / "mem")


def _user_entry(s: MemoryStore) -> MemoryEntry:
    return s.add(MemoryEntry(type=MemoryType.USER, key="home", content="上海", source="user_stated"))


# ── 工具 _remove / _replace ──────────────────────────────────────────────────

def test_tool_remove_cannot_delete_user_stated(tmp_path):
    s = _store(tmp_path)
    e = _user_entry(s)
    tool = MemoryTool(store=s)  # 默认 model_inferred actor
    res = tool._remove({"key": "home"}, MemoryType.USER, session_key="sess")
    assert res.success is False
    assert s.get(e.id) is not None  # 未被删


def test_tool_replace_cannot_overwrite_user_stated(tmp_path):
    s = _store(tmp_path)
    e = _user_entry(s)
    tool = MemoryTool(store=s)
    res = tool._replace(
        {"key": "home", "content": "北京", "source": "model_inferred"},
        MemoryType.USER,
        session_key="sess",
    )
    assert res.success is False
    assert s.get(e.id).content == "上海"


def test_tool_remove_被拒不打tag不写contradiction(tmp_path):
    s = _store(tmp_path)
    e = _user_entry(s)
    tool = MemoryTool(store=s)
    tool._remove({"key": "home"}, MemoryType.USER, session_key="sess")
    kept = s.get(e.id)
    assert MemoryStore.SUSPECTED_CONFLICT_TAG not in kept.tags


# ── reviewer remove ──────────────────────────────────────────────────────────

def test_reviewer_remove_cannot_delete_user_stated(tmp_path):
    s = _store(tmp_path)
    e = _user_entry(s)
    reviewer = MemoryReviewer(provider=MagicMock(), store=s, session_key="sess")
    result = reviewer._execute({"action": "remove", "target": "user", "key": "home"})
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
    server._require_admin_token = MagicMock(return_value=None)  # 授权通过
    api = MemoryAPI(server)
    store = MagicMock()
    server._agent_loop.memory = store
    return api, store


async def _payload(resp):
    return json.loads(resp.body.decode())


@pytest.mark.asyncio
async def test_rest_update_user_stated_denied_without_override(tmp_path):
    api, store = _make_api()
    store.get.return_value = MemoryEntry(
        type=MemoryType.USER, key="home", content="上海", source="user_stated"
    )
    resp = await api.update_entry(_Request(match_info={"id": "x"}, body={"content": "北京"}))
    assert resp.status == 403
    store.update.assert_not_called()


@pytest.mark.asyncio
async def test_rest_delete_user_stated_denied_without_override(tmp_path):
    api, store = _make_api()
    store.get.return_value = MemoryEntry(
        type=MemoryType.USER, key="home", content="上海", source="user_stated"
    )
    resp = await api.delete_entry(_Request(match_info={"id": "x"}))
    assert resp.status == 403
    store.delete.assert_not_called()


@pytest.mark.asyncio
async def test_rest_update_user_stated_allowed_with_override(tmp_path):
    api, store = _make_api()
    store.get.return_value = MemoryEntry(
        type=MemoryType.USER, key="home", content="上海", source="user_stated"
    )
    store.update.return_value = MemoryEntry(
        type=MemoryType.USER, key="home", content="北京", source="user_stated"
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
        type=MemoryType.USER, key="home", content="上海", source="user_stated"
    )
    store.delete.return_value = True
    resp = await api.delete_entry(
        _Request(match_info={"id": "x"}, query={"override": "true"})
    )
    assert resp.status == 200
    store.delete.assert_called_once()
