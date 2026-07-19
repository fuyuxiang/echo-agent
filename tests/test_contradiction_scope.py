from __future__ import annotations

import inspect

import pytest

from echo_agent.memory import consolidator as cons
from echo_agent.memory.contradiction import ContradictionDetector
from echo_agent.memory.service import ActorContext, MemoryService
from echo_agent.memory.store import MemoryStore
from echo_agent.memory.types import Contradiction, MemoryEntry, MemoryType
from echo_agent.storage.sqlite import SQLiteBackend


@pytest.mark.asyncio
async def test_detector_resolve_via_service_invalidates(tmp_path):
    # detector.resolve 的 mark_superseded 改走 service maintenance 通道后,
    # 应触发 service 的失效钩子(此前直连 store 写不触发失效,冻结快照/预取
    # 会跨轮继续注入已被取代的败者条目)。
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

    await detector.resolve("c1", "b_wins", winner_id=winner.id)

    assert store.get(loser.id).superseded_by == winner.id
    # mark_superseded 经 service 触发失效
    assert calls, "detector.resolve 的裁决未经 service 触发失效"


def test_step3_narrows_candidates_by_scope():
    src = inspect.getsource(cons.MemoryConsolidator.sleep_consolidate)
    # 矛盾检测的比较集合按 memory_scope 收窄,不再无条件全库 _entries.values()
    assert "list_all(session_key=memory_scope" in src or "session_key=memory_scope" in src


def test_auto_resolve_requires_same_scope():
    src = inspect.getsource(cons.MemoryConsolidator._auto_resolve_same_key)
    # 同 key 还须同 scope 才裁决,避免跨 scope supersede
    assert "_same_scope" in src
