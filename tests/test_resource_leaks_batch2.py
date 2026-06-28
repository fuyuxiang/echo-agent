from __future__ import annotations

import asyncio
from datetime import datetime, timedelta

import pytest


@pytest.mark.asyncio
async def test_expire_session_loads_on_cache_miss(tmp_path):
    """SQLite 模式下,过期会话即使不在内存缓存,cleanup_expired 也应落库为 expired。"""
    from echo_agent.session.manager import SessionManager
    from echo_agent.storage.sqlite import SQLiteBackend

    backend = SQLiteBackend(tmp_path / "sessions.db")
    await backend.initialize()
    try:
        mgr = SessionManager(
            sessions_dir=tmp_path / "sessions",
            storage=backend,
            expiry_hours=1,
        )
        # 造一个 active 会话并落库,updated_at 设为 2 小时前(已过期)。
        sess = await mgr.get_or_create("telegram:c1")
        sess.updated_at = datetime.now() - timedelta(hours=2)
        await mgr.save(sess)
        # 清空内存缓存,模拟长驻进程里该 key 早已淘汰出缓存。
        mgr._cache.clear()

        count = await mgr.cleanup_expired()

        assert count == 1
        data = await backend.load_session("telegram:c1")
        assert data is not None
        assert data["status"] == "expired"
    finally:
        await backend.close()


@pytest.mark.asyncio
async def test_evict_oldest_cleans_vector_index(tmp_path):
    """容量淘汰应像 delete() 一样清理向量索引,不留 FAISS 孤儿向量。"""
    from echo_agent.memory.store import MemoryStore
    from echo_agent.memory.types import MemoryEntry, MemoryType, MemoryTier

    removed: list[str] = []

    class _VecIndex:
        async def remove(self, embedding_id):
            removed.append(embedding_id)

    store = MemoryStore(memory_dir=tmp_path / "mem", max_user=1)
    store.set_vector_index(_VecIndex())

    e1 = MemoryEntry(type=MemoryType.USER, tier=MemoryTier.SEMANTIC,
                     key="k1", content="first entry", source_session="s")
    e1.embedding_id = "emb-1"
    e2 = MemoryEntry(type=MemoryType.USER, tier=MemoryTier.SEMANTIC,
                     key="k2", content="second entry", source_session="s")
    e2.embedding_id = "emb-2"

    store.add(e1)
    store.add(e2)  # max_user=1, 触发对 e1 的淘汰

    await asyncio.sleep(0.05)  # 让 _cleanup_deleted 调度的异步任务跑完
    assert "emb-1" in removed


@pytest.mark.asyncio
async def test_put_memory_snapshot_bounded_by_lru():
    """快照写入经 put_memory_snapshot -> _lru_put,字典不超 _max_cached_sessions,最旧被逐出。"""
    from collections import OrderedDict
    from echo_agent.agent.loop import AgentLoop

    loop = AgentLoop.__new__(AgentLoop)  # 绕过重 __init__
    import asyncio as _asyncio
    loop._state_lock = _asyncio.Lock()
    loop._memory_snapshots = OrderedDict()
    loop._memory_snapshot_ids = OrderedDict()
    loop._max_cached_sessions = 3

    for i in range(5):
        await loop.put_memory_snapshot(f"s{i}", f"snap{i}")

    assert len(loop._memory_snapshots) == 3
    assert "s0" not in loop._memory_snapshots  # 最旧被逐出
    assert "s4" in loop._memory_snapshots      # 最新保留
    assert len(loop._memory_snapshot_ids) == 3  # 并行 id 集 LRU 同样受限


def test_trace_files_pruned_to_limit(tmp_path):
    """flush 超过上限个 trace 后,目录只保留最近 N 个 trace_*.json。"""
    from echo_agent.observability.monitor import TraceLogger

    tracer = TraceLogger(logs_dir=tmp_path, enabled=True, max_trace_files=3)
    for i in range(5):
        tracer.start_span(trace_id=f"t{i}", span_id=f"sp{i}", name="x", kind="agent")
        tracer.flush_trace(f"t{i}")

    files = sorted(tmp_path.glob("trace_*.json"))
    assert len(files) == 3
    names = {f.name for f in files}
    assert "trace_t0.json" not in names  # 最旧被裁
    assert "trace_t4.json" in names      # 最新保留


def test_trace_prune_disabled_when_limit_non_positive(tmp_path):
    """max_trace_files <= 0 时不裁剪(禁用轮转),不误删。"""
    from echo_agent.observability.monitor import TraceLogger

    tracer = TraceLogger(logs_dir=tmp_path, enabled=True, max_trace_files=0)
    for i in range(4):
        tracer.start_span(trace_id=f"t{i}", span_id=f"sp{i}", name="x", kind="agent")
        tracer.flush_trace(f"t{i}")

    assert len(list(tmp_path.glob("trace_*.json"))) == 4
