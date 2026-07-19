"""R1 Task2: MemoryTool 改走 MemoryService 后的行为等价用例。

工具不再直接调 store 写方法,而是构造 model actor 的 ActorContext 后 await service。
本文件锁定迁移后必须保持的三条语义:remove 仍受 provenance 守卫、add 新 key 成功、
service 注入替代裸 store。
"""

import pytest

from echo_agent.memory.store import MemoryStore
from echo_agent.memory.service import MemoryService
from echo_agent.agent.tools.memory import MemoryTool
from echo_agent.tools.base import ToolExecutionContext


def _tool(tmp_path):
    store = MemoryStore(memory_dir=tmp_path / "mem", scope_policy="session")
    return MemoryTool(service=MemoryService(store)), store


@pytest.mark.asyncio
async def test_tool_remove_still_blocks_user_stated(tmp_path):
    tool, store = _tool(tmp_path)
    ctx = ToolExecutionContext(session_key="s", memory_scope="scope1")
    await tool.execute(
        {"action": "add", "target": "user", "key": "home", "content": "上海", "source": "user_stated"},
        ctx,
    )
    res = await tool.execute({"action": "remove", "target": "user", "key": "home"}, ctx)
    assert res.success is False


@pytest.mark.asyncio
async def test_tool_add_new_key_succeeds(tmp_path):
    tool, store = _tool(tmp_path)
    ctx = ToolExecutionContext(session_key="s", memory_scope="scope1")
    res = await tool.execute(
        {"action": "add", "target": "user", "key": "lang", "content": "喜欢Python"},
        ctx,
    )
    assert res.success is True
    assert store.find_by_key("lang", session_key="scope1").source == "model_inferred"


@pytest.mark.asyncio
async def test_tool_replace_lower_provenance_rejected(tmp_path):
    tool, store = _tool(tmp_path)
    ctx = ToolExecutionContext(session_key="s", memory_scope="scope1")
    await tool.execute(
        {"action": "add", "target": "user", "key": "home", "content": "上海", "source": "user_stated"},
        ctx,
    )
    res = await tool.execute(
        {"action": "replace", "target": "user", "key": "home", "content": "北京", "source": "model_inferred"},
        ctx,
    )
    assert res.success is False
    assert store.find_by_key("home", session_key="scope1").content == "上海"
