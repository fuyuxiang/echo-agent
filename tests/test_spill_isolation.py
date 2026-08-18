"""spill 产物的会话级隔离:一个会话不得取回另一个会话的产物。

产物路径出现在模型可见的 notice 里,所以它必须被当成公开信息:会话 A 可能
把它复述进某条消息,或模型自己在多会话上下文里串了线。授权只能建立在
ctx.session_key 上——"说得出这个路径"绝不构成许可。

多用户渠道下这条失守就是跨租户数据泄露,故这里用两个租户的真实文本做端到端
验证,而不是只断言某个内部函数返回了 None。
"""

from __future__ import annotations

import asyncio
from pathlib import Path

import pytest

from echo_agent.agent.tools.document import ReadDocumentTool
from echo_agent.agent.tools.filesystem import ListDirTool, ReadFileTool
from echo_agent.agent.tools.read_spill import ReadSpillTool
from echo_agent.agent.tools.search import SearchFilesTool
from echo_agent.agent.tools.send_file import SendFileTool
from echo_agent.spill.store import SpillStore
from echo_agent.tools.base import ToolExecutionContext

SECRET = "TENANT_B_BEARER_TOKEN_zzz999"


def _ctx(session_key: str) -> ToolExecutionContext:
    return ToolExecutionContext(execution_id="e", trace_id="t", session_key=session_key)


@pytest.fixture()
def env(tmp_path: Path):
    """workspace 内含 spill 根(与默认 data/spill 布局一致),外加两个租户的产物。"""
    workspace = tmp_path / "ws"
    spill_root = workspace / "data" / "spill"
    spill_root.mkdir(parents=True)
    store = SpillStore(spill_root)
    victim = store.save("tenant-b", "exec", SECRET * 50)
    attacker = store.save("tenant-a", "exec", "TENANT_A_OWN_OUTPUT" * 50)
    return workspace, spill_root, victim, attacker


def _run(coro):
    return asyncio.run(coro)


# ── 通用文件工具:一律拒绝,不论属于谁 ────────────────────────────────────

def test_read_file_refuses_other_sessions_artifact(env):
    workspace, spill_root, victim, _ = env
    tool = ReadFileTool(str(workspace), spill_root=spill_root)
    res = _run(tool.execute({"path": str(victim)}, _ctx("tenant-a")))
    assert not res.success
    assert SECRET not in res.output
    assert "read_spill" in res.error


def test_read_file_refuses_even_own_artifact(env):
    """自己的产物也走 read_spill。

    通用工具没有会话概念,放行"自己的"就意味着它得先判断归属,而它判断不了。
    留一条按路径放行的缝,等于把整个隔离降级成"模型自觉"。
    """
    workspace, spill_root, _, attacker = env
    tool = ReadFileTool(str(workspace), spill_root=spill_root)
    res = _run(tool.execute({"path": str(attacker)}, _ctx("tenant-a")))
    assert not res.success
    assert "read_spill" in res.error


def test_search_files_cannot_reach_into_spill_root(env):
    """直接把搜索根指向 spill 目录:拒。"""
    workspace, spill_root, _, _ = env
    tool = SearchFilesTool(str(workspace), spill_root=spill_root)
    res = _run(tool.execute({"pattern": "TENANT_B", "path": str(spill_root)}, _ctx("tenant-a")))
    assert not res.success
    assert SECRET not in res.output


def test_search_files_prunes_spill_when_scanning_workspace(env):
    """已复现过的那条路径:搜索工作区时顺带扫出别人的产物内容。

    spill 默认就在工作区内,所以这不需要模型有意越权——一次 path="." 的普通
    搜索就够了。闸门只挡"搜索根是 spill",挡不住"搜索根包含 spill",故遍历
    时必须剪枝。
    """
    workspace, spill_root, _, _ = env
    (workspace / "app.py").write_text("print('hello')\n", encoding="utf-8")
    tool = SearchFilesTool(str(workspace), spill_root=spill_root)
    res = _run(tool.execute({"pattern": "TENANT_B_BEARER", "path": "."}, _ctx("tenant-a")))
    assert res.success
    assert SECRET not in res.output
    assert "No matches found." in res.output


def test_glob_search_does_not_enumerate_artifacts(env):
    workspace, spill_root, _, _ = env
    tool = SearchFilesTool(str(workspace), spill_root=spill_root)
    res = _run(tool.execute({"pattern": "*.txt", "mode": "glob", "path": "."}, _ctx("tenant-a")))
    assert res.success
    assert "session-" not in res.output


def test_list_dir_cannot_enumerate_sessions(env):
    """列举 spill 根会泄漏存在哪些会话、各自产出多少。"""
    workspace, spill_root, _, _ = env
    tool = ListDirTool(str(workspace), spill_root=spill_root)
    res = _run(tool.execute({"path": str(spill_root)}, _ctx("tenant-a")))
    assert not res.success
    assert "session-" not in res.output


def test_read_document_refuses_artifact(env):
    """产物是 .txt,read_document 认得它——同样是一条会话无关的读取路径。"""
    workspace, spill_root, victim, _ = env
    tool = ReadDocumentTool(str(workspace), spill_root=spill_root)
    res = _run(tool.execute({"path": str(victim)}, _ctx("tenant-a")))
    assert not res.success
    assert SECRET not in res.output


def test_send_file_refuses_artifact(env):
    """这条最严重:终点是把文件当附件投进聊天,内容直接离开进程。"""
    workspace, spill_root, victim, _ = env
    sent: list = []
    tool = SendFileTool(str(workspace), publish_fn=lambda ev: sent.append(ev),
                        spill_root=spill_root)
    res = _run(tool.execute(
        {"file_path": str(victim), "channel": "cli", "chat_id": "c1"}, _ctx("tenant-a")))
    assert not res.success
    assert sent == []


# ── read_spill:按 ctx.session_key 授权 ──────────────────────────────────────

def test_read_spill_retrieves_own_artifact(env):
    _, spill_root, _, attacker = env
    tool = ReadSpillTool(spill_root=spill_root)
    res = _run(tool.execute({"path": str(attacker)}, _ctx("tenant-a")))
    assert res.success
    assert "TENANT_A_OWN_OUTPUT" in res.output


def test_read_spill_refuses_other_sessions_artifact(env):
    """核心断言:路径完全正确、文件确实存在,但会话不对,必须拒。"""
    _, spill_root, victim, _ = env
    tool = ReadSpillTool(spill_root=spill_root)
    res = _run(tool.execute({"path": str(victim)}, _ctx("tenant-a")))
    assert not res.success
    assert SECRET not in res.output


def test_read_spill_refuses_traversal_into_sibling_session(env):
    """用 ../ 从自己的目录爬到别人的目录:解析后比对父目录,骗不过去。"""
    _, spill_root, victim, _ = env
    tool = ReadSpillTool(spill_root=spill_root)
    rel = f"../{victim.parent.name}/{victim.name}"
    res = _run(tool.execute({"path": rel}, _ctx("tenant-a")))
    assert not res.success
    assert SECRET not in res.output


def test_read_spill_refuses_non_artifact_paths(env, tmp_path):
    """不能拿它当通用读文件工具用:形状不对就拒。"""
    _, spill_root, _, _ = env
    outsider = tmp_path / "secrets.env"
    outsider.write_text("KEY=1", encoding="utf-8")
    tool = ReadSpillTool(spill_root=spill_root)
    res = _run(tool.execute({"path": str(outsider)}, _ctx("tenant-a")))
    assert not res.success
    assert "KEY=1" not in res.output


def test_read_spill_reports_swept_artifact_semantically(env):
    """已被清扫的产物给语义提示,而不是一句模型读不懂的 file not found。"""
    _, spill_root, _, attacker = env
    attacker.unlink()
    tool = ReadSpillTool(spill_root=spill_root)
    res = _run(tool.execute({"path": str(attacker)}, _ctx("tenant-a")))
    assert not res.success
    assert "重新执行" in res.error


def test_read_spill_isolates_empty_session_key(env):
    """session_key 为空时归到 unscoped,不得因此看见别人的产物。"""
    _, spill_root, victim, _ = env
    tool = ReadSpillTool(spill_root=spill_root)
    res = _run(tool.execute({"path": str(victim)}, _ctx("")))
    assert not res.success
    assert SECRET not in res.output
