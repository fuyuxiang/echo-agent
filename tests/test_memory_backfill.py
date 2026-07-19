"""启动回填、孤儿向量治理、update 旧向量替换测试。"""
import asyncio
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


@pytest.mark.asyncio
async def test_merge_reembeds_when_content_changes(store_with_index):
    """同 key 合并改写 content 后，向量必须随之替换，不能停留在旧文本。"""
    store, index = store_with_index
    e = store.add(MemoryEntry(
        type=MemoryType.USER, key="home:city", content="住北京", source="user_stated",
    ))
    await store.flush_pending_embeds()
    old_vec = store._entries[e.id].embedding_id
    assert old_vec

    # 同 key 同级来源、内容不同 → 走 _merge_locked 的 append-version 分支:
    # 旧条目 e.id 置 superseded 保留原内容,新版本承载新内容。
    new_entry = store.add(MemoryEntry(
        type=MemoryType.USER, key="home:city", content="搬到了上海", source="user_stated",
    ))
    assert new_entry.id != e.id and new_entry.content == "搬到了上海"
    assert store._entries[e.id].content == "住北京"          # 旧条目内容保留
    assert store._entries[e.id].superseded_by == new_entry.id  # 旧指向新
    await store.flush_pending_embeds()
    # append 路径清旧向量走 _schedule_vector_removal 的 fire-and-forget 任务,
    # 需等其落地后再断言(update 路径靠 flush 内 replaced_old 同步清,此处不同)。
    if store._pending_storage_tasks:
        await asyncio.gather(*list(store._pending_storage_tasks))
    new_vec = store._entries[new_entry.id].embedding_id
    assert new_vec and new_vec != old_vec
    rows = await store._storage.load_vectors_all()
    ids = {r["id"] for r in rows}
    assert new_vec in ids and old_vec not in ids  # 旧向量已清,只 ACTIVE 进索引
    assert index.count == 1


@pytest.mark.asyncio
async def test_merge_no_reembed_when_content_same(store_with_index):
    """合并未改内容（仅元数据）时不应产生无谓的重嵌队列项。"""
    store, index = store_with_index
    store.add(MemoryEntry(
        type=MemoryType.USER, key="k", content="同样内容", source="user_stated",
    ))
    await store.flush_pending_embeds()
    store._pending_embeds.clear()
    store.add(MemoryEntry(
        type=MemoryType.USER, key="k", content="同样内容", source="user_stated",
        importance=0.9,
    ))
    # 内容未变 → 命中去重直接返回，不入队。
    assert store._pending_embeds == []


@pytest.mark.asyncio
async def test_circuit_open_stops_enqueue_and_flush(store_with_index):
    """熔断跳闸后：不再入队新条目，flush 直接短路不空跑。"""
    store, index = store_with_index

    class _TrippedFn:
        tripped = True
        async def __call__(self, text):
            return []

    store.set_embed_fn(_TrippedFn())
    store.add(MemoryEntry(type=MemoryType.USER, key="k", content="熔断期间写入"))
    assert store._pending_embeds == []          # 未入队
    assert await store.flush_pending_embeds() == 0


@pytest.mark.asyncio
async def test_flush_is_single_flight(store_with_index):
    """并发触发 flush 时靠 _flush_lock 串行，同一条目只生成一个向量。"""
    import asyncio

    store, index = store_with_index
    calls = {"n": 0}
    orig_embed = fake_embed

    async def slow_embed(text: str) -> list[float]:
        calls["n"] += 1
        await asyncio.sleep(0.02)   # 拉长窗口逼出并发
        return await orig_embed(text)

    store.set_embed_fn(slow_embed)
    e = store.add(MemoryEntry(type=MemoryType.USER, key="k", content="唯一条目"))

    # 两个 flush 并发；锁应让第二个在队列清空后空转返回。
    await asyncio.gather(store.flush_pending_embeds(), store.flush_pending_embeds())
    assert calls["n"] == 1                       # 只嵌了一次
    assert index.count == 1                       # 矩阵单行，无重复/孤儿
    assert store._entries[e.id].embedding_id
