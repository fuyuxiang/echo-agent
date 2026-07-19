from __future__ import annotations

import inspect
import json
from unittest.mock import AsyncMock, MagicMock

import pytest

from echo_agent.memory import reflection as refl
from echo_agent.memory.reflection import ReflectionEngine
from echo_agent.memory.service import MemoryService
from echo_agent.memory.store import MemoryStore
from echo_agent.memory.types import MemoryEntry, MemoryType


def _llm_returning(tool_name: str, args: dict):
    resp = MagicMock()
    tc = MagicMock()
    tc.name = tool_name
    tc.arguments = json.dumps(args)
    resp.tool_calls = [tc]
    return AsyncMock(return_value=resp)


@pytest.mark.asyncio
async def test_reflection_supersede_via_service_invalidates(tmp_path):
    """reflection 裁决 mark_superseded 须经 service 触发失效回调,
    否则共享该 scope 的其它 session 会读到旧值。"""
    calls: list[tuple[str, bool]] = []

    async def _inval(scope, g):
        calls.append((scope, g))

    store = MemoryStore(tmp_path / "memory")
    service = MemoryService(store, invalidate_fn=_inval)
    # 一对同 scope 冲突:b(user_stated) 顶掉 a(model_inferred),越过优先级地板。
    a = store.add(MemoryEntry(
        type=MemoryType.USER, key="home:city", content="住北京",
        source="model_inferred", source_session="s1",
        tags=[MemoryStore.SUSPECTED_CONFLICT_TAG],
    ))
    b = MemoryEntry(
        type=MemoryType.USER, key="home:addr", content="搬到上海",
        source="user_stated", source_session="s1",
        tags=[MemoryStore.SUSPECTED_CONFLICT_TAG],
    )
    store._entries[b.id] = b  # 绕过守卫构造并存冲突对
    detector = MagicMock()
    detector.get_unresolved = AsyncMock(return_value=[])
    detector.resolve = AsyncMock()
    llm = _llm_returning("adjudicate", {"verdict": "b_wins", "explanation": "时效替代"})
    engine = ReflectionEngine(service, llm_call=llm, contradiction_detector=detector)

    stats = await engine.resolve_conflicts()

    assert stats["resolved"] == 1
    assert store.get(a.id).is_superseded
    # 裁决经 service 精简写序,失效回调被触发(mark_superseded + 清 tag 均会失效)。
    assert calls, "mark_superseded 未经 service 触发 invalidate_fn"


def test_run_accepts_memory_scope():
    sig = inspect.signature(refl.ReflectionEngine.run)
    assert "memory_scope" in sig.parameters


def test_prefix_groups_filters_by_scope():
    src = inspect.getsource(refl.ReflectionEngine._prefix_groups)
    # 取数按 scope 收窄,不再无参全库 list_all()
    assert "session_key" in src


def test_ask_distill_uses_memory_scope_for_source_session():
    src = inspect.getsource(refl.ReflectionEngine._ask_distill)
    # 产物 source_session 用传入 memory_scope(回退 sample),不再无条件继承 entries[0]
    assert "memory_scope" in src
