"""R1 Task2: MemoryTool 改走 MemoryService 后的行为等价用例。

工具不再直接调 store 写方法,而是构造 model actor 的 ActorContext 后 await service。
本文件锁定迁移后必须保持的三条语义:remove 仍受 provenance 守卫、add 新 key 成功、
service 注入替代裸 store。
"""

import pytest

from echo_agent.memory.contradiction import ContradictionDetector
from echo_agent.memory.store import MemoryStore
from echo_agent.memory.service import MemoryService
from echo_agent.memory.types import Contradiction, MemoryEntry, MemoryType
from echo_agent.storage.sqlite import SQLiteBackend
from echo_agent.agent.tools.memory import MemoryTool
from echo_agent.tools.base import ToolExecutionContext


def _tool(tmp_path):
    store = MemoryStore(memory_dir=tmp_path / "mem", scope_policy="session")
    return MemoryTool(service=MemoryService(store)), store


@pytest.mark.asyncio
async def test_resolve_contradiction_triggers_global_invalidation(tmp_path):
    # 裁决把败者标记为 superseded(store.mark_superseded),若不失效,
    # 冻结快照/预取会跨轮继续注入已被取代的旧条目。裁决对所有会话可见,须全局失效。
    calls = []

    async def _inval(scope, global_scope):
        calls.append((scope, global_scope))

    class _Detector:
        async def get_unresolved(self, limit=10):
            return [Contradiction(id="c1", memory_id_a="a", memory_id_b="b",
                                  description="conflict")]
        async def resolve(self, cid, resolution, winner_id=None):
            return None

    store = MemoryStore(memory_dir=tmp_path / "mem", scope_policy="session")
    service = MemoryService(store, invalidate_fn=_inval)
    tool = MemoryTool(service=service, contradiction_detector=_Detector())

    res = await tool.execute(
        {"action": "resolve_contradiction", "contradiction_id": "c1", "winner_id": "a"}
    )
    assert res.success is True
    assert any(global_scope is True for _, global_scope in calls), calls


@pytest.mark.asyncio
async def test_resolve_contradiction_via_wired_detector_invalidates_once(tmp_path):
    """detector 已装配 service 时:裁决经 detector→service.mark_superseded→_finalize
    已触发失效,工具不得再显式 invalidate 二次失效。

    装配后 detector.resolve 的 mark_superseded 走 service maintenance 通道会失效一次;
    此前工具随后又 service.invalidate(global_scope=True) 造成同一裁决失效两次。
    修后:装配 service 的 detector 只失效一次(由 detector 触发),且败者确被 superseded。
    """
    calls: list[tuple[str, bool]] = []

    async def _inval(scope, g):
        calls.append((scope, g))

    storage = SQLiteBackend(tmp_path / "db.sqlite")
    await storage.initialize()
    store = MemoryStore(memory_dir=tmp_path / "mem")
    service = MemoryService(store, invalidate_fn=_inval)

    winner = store.add(MemoryEntry(type=MemoryType.USER, key="home", content="北京",
                                   source="user_stated", source_session="s1"))
    loser = store.add(MemoryEntry(type=MemoryType.USER, key="home", content="上海",
                                  source="user_stated", source_session="s1"))

    detector = ContradictionDetector(storage=storage, store=store, service=service)
    c = Contradiction(id="c1", memory_id_a=loser.id, memory_id_b=winner.id, description="x")
    await detector.store_contradiction(c)

    tool = MemoryTool(service=service, contradiction_detector=detector)
    res = await tool.execute(
        {"action": "resolve_contradiction", "contradiction_id": "c1", "winner_id": winner.id}
    )
    assert res.success is True
    # 裁决确实生效:败者被标记 superseded(证明失效未被删没)
    assert store.get(loser.id).superseded_by == winner.id
    # 只失效一次(detector 经 service.mark_superseded 触发),工具不再二次失效
    assert len(calls) == 1, calls


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
