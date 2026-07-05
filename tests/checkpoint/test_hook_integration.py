import shutil
from dataclasses import dataclass
from pathlib import Path

import pytest

from echo_agent.checkpoint.hook import make_pre_tool_checkpoint_hook
from echo_agent.checkpoint.manager import CheckpointManager
from echo_agent.checkpoint.store import ShadowGitStore

pytestmark = pytest.mark.skipif(shutil.which("git") is None, reason="git not installed")


@dataclass
class _Ctx:
    trace_id: str = ""


async def _mgr(tmp_path: Path) -> CheckpointManager:
    store = ShadowGitStore(tmp_path / "store")
    await store.ensure_initialized()
    ws = tmp_path / "ws"
    ws.mkdir(exist_ok=True)
    return CheckpointManager(store=store, workspace=ws)


@pytest.mark.asyncio
async def test_hook_snapshots_before_write_tool(tmp_path: Path):
    mgr = await _mgr(tmp_path)
    (mgr._workspace / "a.txt").write_text("v1")
    cb = make_pre_tool_checkpoint_hook(mgr)
    await cb("write_file", {"path": "a.txt", "content": "v2"}, _Ctx(trace_id="t1"))
    assert len(await mgr.list_snapshots()) == 1


@pytest.mark.asyncio
async def test_hook_ignores_non_write_tool(tmp_path: Path):
    mgr = await _mgr(tmp_path)
    (mgr._workspace / "a.txt").write_text("v1")
    cb = make_pre_tool_checkpoint_hook(mgr)
    await cb("read_file", {"path": "a.txt"}, _Ctx(trace_id="t1"))
    assert await mgr.list_snapshots() == []


@pytest.mark.asyncio
async def test_hook_snapshots_before_edit_file_tool(tmp_path: Path):
    # regression: the real edit tool is named "edit_file", not "edit"
    mgr = await _mgr(tmp_path)
    (mgr._workspace / "a.txt").write_text("v1")
    cb = make_pre_tool_checkpoint_hook(mgr)
    await cb("edit_file", {"path": "a.txt", "old_string": "v1", "new_string": "v2"}, _Ctx(trace_id="t1"))
    assert len(await mgr.list_snapshots()) == 1


@pytest.mark.asyncio
async def test_hook_dedups_across_write_tools_same_turn(tmp_path: Path):
    mgr = await _mgr(tmp_path)
    (mgr._workspace / "a.txt").write_text("v1")
    cb = make_pre_tool_checkpoint_hook(mgr)
    await cb("write_file", {}, _Ctx(trace_id="t1"))
    await cb("patch", {}, _Ctx(trace_id="t1"))
    assert len(await mgr.list_snapshots()) == 1
