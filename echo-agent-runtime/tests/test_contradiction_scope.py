from __future__ import annotations

import inspect

import pytest
import pytest_asyncio

from echo_agent.memory import consolidator as cons
from echo_agent.memory.contradiction import ContradictionDetector
from echo_agent.memory.service import MemoryService
from echo_agent.memory.store import MemoryStore
from echo_agent.memory.types import Contradiction, MemoryEntry, MemoryType
from echo_agent.storage.sqlite import SQLiteBackend


@pytest_asyncio.fixture
async def sqlite_storage(tmp_path):
    storage = SQLiteBackend(tmp_path / "db.sqlite")
    await storage.initialize()
    try:
        yield storage
    finally:
        await storage.close()


@pytest.mark.asyncio
async def test_detector_resolve_via_service_invalidates(tmp_path, sqlite_storage):
    # detector.resolve 的 mark_superseded 改走 service maintenance 通道后,
    # 应触发 service 的失效钩子(此前直连 store 写不触发失效,冻结快照/预取
    # 会跨轮继续注入已被取代的败者条目)。
    calls: list[tuple[str, bool]] = []

    async def _inval(scope, g):
        calls.append((scope, g))

    storage = sqlite_storage
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


@pytest.mark.asyncio
async def test_get_unresolved_filters_cross_scope(tmp_path, sqlite_storage):
    # A: s1 只应看到两端都对 s1 可见的矛盾对,含 s2 条目的对不泄露。
    storage = sqlite_storage
    store = MemoryStore(memory_dir=tmp_path / "mem", scope_policy="session")
    a1 = store.add(MemoryEntry(type=MemoryType.USER, key="home", content="北京",
                               source="user_stated", source_session="s1"))
    a2 = store.add(MemoryEntry(type=MemoryType.USER, key="home", content="上海",
                               source="user_stated", source_session="s1"))
    b1 = store.add(MemoryEntry(type=MemoryType.USER, key="lang", content="Python",
                               source="user_stated", source_session="s2"))
    b2 = store.add(MemoryEntry(type=MemoryType.USER, key="lang", content="Rust",
                               source="user_stated", source_session="s2"))

    detector = ContradictionDetector(storage=storage, store=store)
    await detector.store_contradiction(
        Contradiction(id="c_s1", memory_id_a=a1.id, memory_id_b=a2.id, description="s1 conflict"))
    await detector.store_contradiction(
        Contradiction(id="c_s2", memory_id_a=b1.id, memory_id_b=b2.id, description="s2 conflict"))

    s1_view = await detector.get_unresolved(limit=20, memory_scope="s1")
    assert {c.id for c in s1_view} == {"c_s1"}
    s2_view = await detector.get_unresolved(limit=20, memory_scope="s2")
    assert {c.id for c in s2_view} == {"c_s2"}
    # 全库语义(内部维护)仍见全部
    all_view = await detector.get_unresolved(limit=20)
    assert {c.id for c in all_view} == {"c_s1", "c_s2"}


@pytest.mark.asyncio
async def test_get_unresolved_scope_no_starvation(tmp_path, sqlite_storage):
    # A: s1 有 1 条、s2 有 100 条时,limit=10 仍必须返回 s1 那条(过滤后再截,不饥饿)。
    storage = sqlite_storage
    store = MemoryStore(memory_dir=tmp_path / "mem", scope_policy="session")
    detector = ContradictionDetector(storage=storage, store=store)

    # 先塞 100 条 s2 矛盾(created_at 更晚,排在前),再塞 1 条 s1。
    for i in range(100):
        x = store.add(MemoryEntry(type=MemoryType.USER, key=f"k{i}", content="a",
                                  source="user_stated", source_session="s2"))
        y = store.add(MemoryEntry(type=MemoryType.USER, key=f"k{i}", content="b",
                                  source="user_stated", source_session="s2"))
        await detector.store_contradiction(
            Contradiction(id=f"s2_{i}", memory_id_a=x.id, memory_id_b=y.id, description="x"))
    p = store.add(MemoryEntry(type=MemoryType.USER, key="home", content="北京",
                              source="user_stated", source_session="s1"))
    q = store.add(MemoryEntry(type=MemoryType.USER, key="home", content="上海",
                              source="user_stated", source_session="s1"))
    await detector.store_contradiction(
        Contradiction(id="s1_only", memory_id_a=p.id, memory_id_b=q.id, description="s1"))

    s1_view = await detector.get_unresolved(limit=10, memory_scope="s1")
    assert [c.id for c in s1_view] == ["s1_only"]


def test_empty_scope_user_fail_closed(tmp_path):
    # B: 空 source_session 的 USER 条目在 session 策略下不可见(fail-closed);
    # ENVIRONMENT 空 scope 仍可见;global tag 仍可见。
    store = MemoryStore(memory_dir=tmp_path / "mem", scope_policy="session")
    user_orphan = MemoryEntry(type=MemoryType.USER, key="home", content="北京",
                              source="user_stated", source_session="")
    env_orphan = MemoryEntry(type=MemoryType.ENVIRONMENT, key="os", content="linux",
                             source="observed", source_session="")
    global_user = MemoryEntry(type=MemoryType.USER, key="lang", content="zh",
                              source="user_stated", source_session="", tags=["global"])
    assert store.is_visible_in_session(user_orphan, "s1") is False
    assert store.is_visible_in_session(env_orphan, "s1") is True
    assert store.is_visible_in_session(global_user, "s1") is True


@pytest.mark.asyncio
async def test_resolve_supersede_failure_keeps_row_open(tmp_path, sqlite_storage):
    # G: supersede 败者失败 → 矛盾行仍 unresolved、镜像仍 unresolved、败者仍 active;
    # 二次 resolve 成功后三者一致。
    storage = sqlite_storage
    store = MemoryStore(memory_dir=tmp_path / "mem")

    winner = store.add(MemoryEntry(type=MemoryType.USER, key="home", content="北京",
                                   source="user_stated", source_session="s1"))
    loser = store.add(MemoryEntry(type=MemoryType.USER, key="home", content="上海",
                                  source="user_stated", source_session="s1"))

    fail = {"on": True}

    class _FailOnceService:
        async def mark_superseded(self, ctx, entry_id, superseded_by):
            from echo_agent.memory.service import WriteResult
            if fail["on"]:
                return WriteResult(ok=False, entry=None, reason="injected")
            store.mark_superseded(entry_id, superseded_by)
            return WriteResult(ok=True, entry=store.get(entry_id))

    detector = ContradictionDetector(storage=storage, store=store, service=_FailOnceService())
    c = Contradiction(id="c1", memory_id_a=loser.id, memory_id_b=winner.id, description="x")
    await detector.store_contradiction(c)
    assert store.is_unresolved(loser.id)

    # 第一次:supersede 失败 → resolve 返回 False,行/镜像/败者都不动。
    ok = await detector.resolve("c1", "b_wins", winner_id=winner.id)
    assert ok is False
    rows = await storage.fetch_sql(
        "SELECT resolution FROM memory_contradictions WHERE id='c1'")
    assert rows[0]["resolution"] is None       # SQL 行仍 unresolved
    assert store.is_unresolved(loser.id)       # 镜像仍 unresolved
    assert not store.get(loser.id).is_superseded  # 败者仍 active

    # 第二次:supersede 成功 → 三者一致(行关闭、镜像清除、败者 superseded)。
    fail["on"] = False
    ok = await detector.resolve("c1", "b_wins", winner_id=winner.id)
    assert ok is True
    rows = await storage.fetch_sql(
        "SELECT resolution FROM memory_contradictions WHERE id='c1'")
    assert rows[0]["resolution"] == "b_wins"
    assert not store.is_unresolved(loser.id)
    assert store.get(loser.id).superseded_by == winner.id


@pytest.mark.asyncio
async def test_resolve_missing_loser_still_closes_row(tmp_path, sqlite_storage):
    # G: loser 已被并发删除 → resolve 仍能关行(幂等路径)。
    storage = sqlite_storage
    store = MemoryStore(memory_dir=tmp_path / "mem")

    winner = store.add(MemoryEntry(type=MemoryType.USER, key="home", content="北京",
                                   source="user_stated", source_session="s1"))
    loser = store.add(MemoryEntry(type=MemoryType.USER, key="home", content="上海",
                                  source="user_stated", source_session="s1"))
    detector = ContradictionDetector(storage=storage, store=store)
    c = Contradiction(id="c1", memory_id_a=loser.id, memory_id_b=winner.id, description="x")
    await detector.store_contradiction(c)

    # 模拟败者被并发删除。
    store.delete(loser.id)
    ok = await detector.resolve("c1", "b_wins", winner_id=winner.id)
    assert ok is True
    rows = await storage.fetch_sql(
        "SELECT resolution FROM memory_contradictions WHERE id='c1'")
    assert rows[0]["resolution"] == "b_wins"
