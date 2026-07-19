from __future__ import annotations

import asyncio

from echo_agent.agent.tools.memory import MemoryTool
from echo_agent.memory.service import MemoryService
from echo_agent.memory.store import MemoryStore
from echo_agent.tools.base import ToolExecutionContext


# ENV/global 门禁已从工具下沉到 service;工具经 service.allow_env_writes 控制。
# ENVIRONMENT 写允许空 scope;USER 写须带 memory_scope,故统一给一个 ctx。
_CTX = ToolExecutionContext(session_key="s", memory_scope="scope1")


def _tool(tmp_path, allow):
    store = MemoryStore(memory_dir=tmp_path / "mem", scope_policy="session")
    return MemoryTool(service=MemoryService(store, allow_env_writes=allow))


def test_env_write_denied_by_default(tmp_path):
    tool = _tool(tmp_path, allow=False)
    res = asyncio.run(tool.execute(
        {"action": "add", "target": "environment", "key": "env:os", "content": "Linux"},
        _CTX,
    ))
    assert res.success is False
    assert "environment" in (res.error or "").lower()


def test_env_write_allowed_when_enabled(tmp_path):
    tool = _tool(tmp_path, allow=True)
    res = asyncio.run(tool.execute(
        {"action": "add", "target": "environment", "key": "env:os", "content": "Linux"},
        _CTX,
    ))
    assert res.success is True


def test_user_write_unaffected(tmp_path):
    tool = _tool(tmp_path, allow=False)
    res = asyncio.run(tool.execute(
        {"action": "add", "target": "user", "key": "user:city", "content": "上海"},
        _CTX,
    ))
    assert res.success is True


def test_global_tag_write_denied_by_default(tmp_path):
    tool = _tool(tmp_path, allow=False)
    res = asyncio.run(tool.execute(
        {"action": "add", "target": "user", "key": "user:x", "content": "y", "tags": "global"},
        _CTX,
    ))
    assert res.success is False
