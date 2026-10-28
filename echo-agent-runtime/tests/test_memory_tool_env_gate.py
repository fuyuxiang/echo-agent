from __future__ import annotations

import asyncio

from echo_agent.agent.tools.memory import MemoryTool
from echo_agent.memory.service import MemoryService
from echo_agent.memory.store import MemoryStore
from echo_agent.tools import ToolExecutionContext


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


# ── schema 与门禁一致性 ─────────────────────────────────────────────────────
# 门禁关闭时若 schema 仍暴露 target="environment",模型会按 schema 写 ENV、
# 每次被 service 拒一轮,并把英文错误翻译给用户(线上表现:微信收到
# 「环境记忆写入被禁用了,跳过存档」)。schema 必须随门禁收窄。

def _target_prop(tool):
    return tool.parameters_for_channel(None)["properties"]["target"]


def test_schema_hides_environment_when_gate_closed(tmp_path):
    tool = _tool(tmp_path, allow=False)
    assert _target_prop(tool)["enum"] == ["user"]


def test_schema_exposes_environment_when_gate_open(tmp_path):
    tool = _tool(tmp_path, allow=True)
    assert _target_prop(tool)["enum"] == ["user", "environment"]


def test_schema_narrowing_does_not_mutate_class_parameters(tmp_path):
    """收窄走深拷贝:类级 parameters 是共享状态,原地改会污染同进程内
    其他实例(门禁开放的那个)与后续构造。"""
    closed = _tool(tmp_path / "a", allow=False)
    _target_prop(closed)
    from echo_agent.agent.tools.memory import MemoryTool
    assert MemoryTool.parameters["properties"]["target"]["enum"] == ["user", "environment"]
    opened = _tool(tmp_path / "b", allow=True)
    assert _target_prop(opened)["enum"] == ["user", "environment"]


def test_to_schema_reflects_closed_gate(tmp_path):
    """模型实际看到的是 to_schema 的产物,断言这一层而非仅内部方法。"""
    tool = _tool(tmp_path, allow=False)
    schema = tool.to_schema()
    props = schema["function"]["parameters"]["properties"]
    assert props["target"]["enum"] == ["user"]
    # description 必须说明只有 user 可用,不能仍把 environment 描述成一个选项。
    assert "only 'user'" in props["target"]["description"].lower()


def test_rejection_message_points_to_user_retry(tmp_path):
    """模型忽略收窄后的 enum 仍写 ENV 时,错误文案必须给出可执行的下一步,
    否则模型会把它当成故障转述给用户。"""
    tool = _tool(tmp_path, allow=False)
    res = asyncio.run(tool.execute(
        {"action": "add", "target": "environment", "key": "env:os", "content": "Linux"},
        _CTX,
    ))
    assert res.success is False
    err = (res.error or "").lower()
    assert 'target="user"' in err
    assert "do not report this as a failure" in err
