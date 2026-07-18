from __future__ import annotations

import asyncio

from echo_agent.agent.tools.memory import MemoryTool
from echo_agent.memory.store import MemoryStore


def _tool(tmp_path, allow):
    store = MemoryStore(memory_dir=tmp_path / "mem", scope_policy="session")
    return MemoryTool(store=store, allow_environment_writes=allow)


def test_env_write_denied_by_default(tmp_path):
    tool = _tool(tmp_path, allow=False)
    res = asyncio.run(tool.execute(
        {"action": "add", "target": "environment", "key": "env:os", "content": "Linux"},
    ))
    assert res.success is False
    assert "environment" in (res.error or "").lower()


def test_env_write_allowed_when_enabled(tmp_path):
    tool = _tool(tmp_path, allow=True)
    res = asyncio.run(tool.execute(
        {"action": "add", "target": "environment", "key": "env:os", "content": "Linux"},
    ))
    assert res.success is True


def test_user_write_unaffected(tmp_path):
    tool = _tool(tmp_path, allow=False)
    res = asyncio.run(tool.execute(
        {"action": "add", "target": "user", "key": "user:city", "content": "上海"},
    ))
    assert res.success is True


def test_global_tag_write_denied_by_default(tmp_path):
    tool = _tool(tmp_path, allow=False)
    res = asyncio.run(tool.execute(
        {"action": "add", "target": "user", "key": "user:x", "content": "y", "tags": "global"},
    ))
    assert res.success is False
