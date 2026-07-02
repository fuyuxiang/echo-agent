"""启动回填、孤儿向量治理、update 旧向量替换测试。"""
from pathlib import Path

import numpy as np
import pytest
import pytest_asyncio

from echo_agent.memory.store import MemoryStore
from echo_agent.memory.types import MemoryEntry, MemoryType
from echo_agent.memory.vectors import VectorIndex
from echo_agent.storage.sqlite import SQLiteBackend

MODEL = "fastembed:BAAI/bge-small-zh-v1.5"


async def fake_embed(text: str) -> list[float]:
    # 确定性伪嵌入：按文本哈希生成 4 维向量
    h = abs(hash(text))
    return [float((h >> i) & 0xFF) / 255.0 + 0.01 for i in (0, 8, 16, 24)]


@pytest_asyncio.fixture
async def storage(tmp_path: Path) -> SQLiteBackend:
    backend = SQLiteBackend(tmp_path / "test.db")
    await backend.initialize()
    yield backend
    await backend.close()


@pytest_asyncio.fixture
async def store_with_index(tmp_path: Path, storage: SQLiteBackend):
    store = MemoryStore(tmp_path / "memory", storage=storage)
    index = VectorIndex(storage, dimensions=4, model_id=MODEL)
    await index.initialize()
    store.set_vector_index(index)
    store.set_embed_fn(fake_embed)
    return store, index


@pytest.mark.asyncio
async def test_queue_missing_embeds_picks_unembedded(store_with_index):
    store, index = store_with_index
    e1 = store.add(MemoryEntry(type=MemoryType.USER, key="k1", content="记住我喜欢Python"))
    await store.flush_pending_embeds()
    assert store._entries[e1.id].embedding_id
    # 模拟一条从未嵌入的条目（如 embedding 后端宕机期间写入）
    e2 = store.add(MemoryEntry(type=MemoryType.USER, key="k2", content="记住我在北京"))
    store._pending_embeds.clear()  # 模拟重启丢队列
    assert not store._entries[e2.id].embedding_id

    queued = store.queue_missing_embeds()
    assert queued == 1
    await store.flush_pending_embeds()
    assert store._entries[e2.id].embedding_id


@pytest.mark.asyncio
async def test_queue_missing_embeds_includes_stale(store_with_index):
    store, index = store_with_index
    e1 = store.add(MemoryEntry(type=MemoryType.USER, key="k1", content="旧模型嵌入的条目"))
    await store.flush_pending_embeds()
    old_vec = store._entries[e1.id].embedding_id
    # 模拟模型切换：该条目的 source_id 出现在 stale 集合
    queued = store.queue_missing_embeds(stale_source_ids={e1.id})
    assert queued == 1
    await store.flush_pending_embeds()
    new_vec = store._entries[e1.id].embedding_id
    assert new_vec and new_vec != old_vec
    # 旧向量行已删除
    rows = await store._storage.load_vectors_all()
    assert all(r["id"] != old_vec for r in rows)


@pytest.mark.asyncio
async def test_update_replaces_old_vector(store_with_index):
    store, index = store_with_index
    e = store.add(MemoryEntry(type=MemoryType.USER, key="k", content="原始内容"))
    await store.flush_pending_embeds()
    old_vec = store._entries[e.id].embedding_id
    assert old_vec

    store.update(e.id, content="更新后的内容")
    await store.flush_pending_embeds()
    new_vec = store._entries[e.id].embedding_id
    assert new_vec and new_vec != old_vec
    rows = await store._storage.load_vectors_all()
    ids = {r["id"] for r in rows}
    assert new_vec in ids and old_vec not in ids
    assert index.count == 1  # 矩阵里也只有一行


@pytest.mark.asyncio
async def test_scan_orphan_vectors_removes_unreferenced(store_with_index):
    store, index = store_with_index
    e = store.add(MemoryEntry(type=MemoryType.USER, key="k", content="正常条目"))
    await store.flush_pending_embeds()
    # 手工插一条无主向量（模拟历史孤儿）
    orphan = np.array([1.0, 0.0, 0.0, 0.0], dtype=np.float32).tobytes()
    await store._storage.store_vector("v_orphan", "mem_gone", orphan, {}, model=MODEL, dim=4)

    removed = await store.scan_orphan_vectors()
    assert removed == 1
    rows = await store._storage.load_vectors_all()
    assert all(r["id"] != "v_orphan" for r in rows)
    # 正常条目的向量不受影响
    assert any(r["id"] == store._entries[e.id].embedding_id for r in rows)


@pytest.mark.asyncio
async def test_queue_missing_skips_superseded(store_with_index):
    store, index = store_with_index
    e = store.add(MemoryEntry(type=MemoryType.USER, key="k", content="被取代的条目"))
    store._pending_embeds.clear()
    store.mark_superseded(e.id, "winner_id")
    assert store.queue_missing_embeds() == 0
