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
