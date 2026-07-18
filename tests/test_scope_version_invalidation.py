from __future__ import annotations

import asyncio
import inspect


def test_loop_has_scope_versions():
    from echo_agent.agent import loop
    src = inspect.getsource(loop.AgentLoop.__init__)
    assert "_scope_versions" in src


def test_invalidate_bumps_version_not_pop():
    from echo_agent.agent import loop
    src = inspect.getsource(loop.AgentLoop._invalidate_memory_caches)
    # per-scope 分支改为 bump 版本,不再 pop 单个 session key
    assert "_scope_versions" in src


def test_retrieval_cache_entry_has_scope_version():
    from echo_agent.memory.prefetch import RetrievalCacheEntry
    import dataclasses
    names = {f.name for f in dataclasses.fields(RetrievalCacheEntry)}
    assert "scope" in names and "scope_version" in names


def _make_loop_for_meta():
    # 构造一个仅够测缓存上限的 AgentLoop 替身:直接 new 出实例并置最小状态。
    from echo_agent.agent.loop import AgentLoop
    from collections import OrderedDict
    loop = AgentLoop.__new__(AgentLoop)
    loop._state_lock = asyncio.Lock()
    loop._memory_snapshots = OrderedDict()
    loop._memory_snapshot_ids = OrderedDict()
    loop._memory_snapshot_meta = {}
    loop._max_cached_sessions = 3
    return loop


def test_snapshot_meta_bounded_by_lru():
    loop = _make_loop_for_meta()

    async def run():
        for i in range(10):
            await loop.put_memory_snapshot(f"s{i}", "v", frozenset(), "owner", 0)
    asyncio.run(run())
    # meta 不得超出快照上限,且键集与快照一致(无孤儿)
    assert len(loop._memory_snapshot_meta) <= loop._max_cached_sessions
    assert set(loop._memory_snapshot_meta) == set(loop._memory_snapshots)


def test_clear_memory_snapshot_pops_meta():
    loop = _make_loop_for_meta()

    async def run():
        await loop.put_memory_snapshot("sA", "v", frozenset(), "owner", 0)
        await loop._clear_memory_snapshot("sA")
    asyncio.run(run())
    assert "sA" not in loop._memory_snapshot_meta
    assert "sA" not in loop._memory_snapshots


def test_response_stage_prefetch_uses_real_scope_version():
    from echo_agent.agent.pipeline import response_stage
    import inspect
    src = inspect.getsource(response_stage.ResponseStage)
    # prefetch 不再硬编码 scope_version=0,改用注入的 scope_version_fn
    assert "scope_version=0" not in src
    assert "_scope_version_fn" in src
